use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::io::prelude::*;
use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;

use bit_vec::BitVec;
use log::{debug, info, warn};

use super::endgame;
use super::piece_info::PieceInfo;
use crate::common::hash_to_bytes;
use crate::common::File;
use crate::common::Sha1Hash;
use crate::common::Torrent;
use crate::common::BLOCK_LENGTH;
use crate::messages::{BlockReader, Cancel, Have, Request};

// Trait that represents a "store" that writes pieces to file and has the current state of the
// download.
// Maybe can have different "stores", the main one writes to the file system but you can also
// write to network drive, ftp (which would involve more caching in memory to avoid transit delays),
// etc.

pub trait PieceStore: Sized {
    // Create a new PieceStore for torrent.
    fn new(
        torrent: &Torrent,
        piece_info: PieceInfo,
        failed_hash: Sender<(usize, usize)>,
        have_send: Sender<Have>,
    ) -> Result<Self, Box<dyn Error>>;

    fn done(&self) -> bool;

    // Shows which pieces this PieceStore has.
    //
    // Note that there might be a race here; pieces may be done but have not yet been written to
    // the PieceStore.  Also, depending on the implementation, this function may be quite costly
    // in sychronization overhead.
    fn have(&self) -> BitVec;

    // Retrieves the given block at index and offset.
    //
    // Returns None if this PieceStore does not have the block.
    fn get_block(&self, index: usize, offset: usize) -> Option<Vec<u8>>;

    // Retrieves the given piece at index and offset.
    //
    // Returns None if this PieceStore does not have the piece.
    fn get_piece(&self, index: usize) -> Option<Vec<u8>>;

    fn get_block_have(&self, index: usize) -> BitVec;

    fn num_pieces(&self) -> usize;

    fn percent_done(&self) -> f64;

    fn verify_all_files(&self) -> bool;

    fn write(&mut self, piece: CompletedPiece) -> Result<(), std::io::Error>;
}

pub type AllocatedFiles = HashMap<PathBuf, fs::File>;

// Split out filesystem?
pub struct FileSystem {
    failed_hash: Sender<(usize, usize)>,
    have_send: Sender<Have>,
    snapshot_blocks_received: usize,
    pub info: Arc<FileSystemInfo>,
    piece_info: PieceInfo,
    sender: Option<Sender<CompletedPiece>>,
    write_cache: HashMap<usize, PieceInFlight>, // TODO: this can get arbitrarily large if we get many pieces in flight (unlikely at least)
    write_thread: Option<JoinHandle<()>>,
}

impl FileSystem {
    pub fn endgame_reconcile(&self, sent: &mut HashMap<usize, BitVec>) -> (usize, Vec<Cancel>) {
        endgame::reconcile(|x| self.get_block_have(x), self.have(), sent)
    }

    // Vec<(PieceIndex, BlockIndex)>, Vec<PieceIndex>
    pub fn endgame_get_unreceived_blocks(&self) -> Vec<Request> {
        let mut result = Vec::new();
        info!(
            "Endgame unreceived: Pieces in flight: {}",
            self.write_cache.len()
        );
        for piece_in_flight in self.write_cache.values() {
            for (block_index, have) in piece_in_flight.have.iter().enumerate() {
                if !have {
                    result.push(Request::new(
                        block_index,
                        piece_in_flight.index,
                        self.piece_info,
                    ));
                }
            }
        }
        for (piece_index, value) in self.info.have.iter().enumerate() {
            if !value.load(Ordering::Relaxed) && !self.write_cache.contains_key(&piece_index) {
                for block_index in 0..self.piece_info.get_num_blocks(piece_index) {
                    result.push(Request::new(block_index, piece_index, self.piece_info));
                }
            }
        }
        result
    }

    fn get_bytes(&self, index: usize, offset: usize, num_bytes: usize) -> Vec<u8> {
        let mut result = Vec::new();
        result.resize(num_bytes, 0u8);
        let mut current_index = 0;
        let begin_byte = index * self.info.piece_length + offset;
        let mut file_begin = 0;
        for file_info in &self.info.files_info {
            let file_end = file_begin + file_info.length;
            if begin_byte >= file_end {
                file_begin = file_end;
                continue;
            }
            let file_temp = self.info.files.lock().unwrap();
            let mut file = file_temp.get(file_info.path.as_path()).unwrap();
            let piece_start_byte_in_file = if begin_byte < file_begin {
                0
            } else {
                begin_byte - file_begin
            };
            file.seek(SeekFrom::Start(piece_start_byte_in_file as u64))
                .unwrap();
            let read = file.read(&mut result[current_index..]).unwrap();
            current_index += read;
            if current_index == num_bytes {
                return result;
            }
            file_begin = file_end;
        }
        result.resize(current_index, 0);
        result
    }

    fn get_piece_length(&self, index: usize) -> usize {
        if index == self.info.piece_hashes.len() - 1 {
            self.info.last_piece_length
        } else {
            self.info.piece_length
        }
    }

    pub fn write_block<T: Read>(&mut self, block: BlockReader<T>) -> Result<(), std::io::Error> {
        let piece_index = block.piece_index();
        if self.info.have[piece_index].load(Ordering::Relaxed) {
            debug!("Got block for already received piece");
            return Ok(());
        }
        self.snapshot_blocks_received += 1;
        let piece_info = self.piece_info;
        let piece = self
            .write_cache
            .entry(block.piece_index())
            .or_insert_with(|| PieceInFlight::new(block.piece_index(), piece_info));
        if let Ok(Some(completed)) = piece.add_block(block) {
            self.write(completed)?;
            self.write_cache.remove(&piece_index);
        }
        Ok(())
    }
}

impl PieceStore for FileSystem {
    fn new(
        torrent: &Torrent,
        piece_info: PieceInfo,
        failed_hash: Sender<(usize, usize)>,
        have_send: Sender<Have>,
    ) -> Result<Self, Box<dyn Error>> {
        let num_pieces = torrent.metainfo.pieces.len();
        let mut allocated_files = AllocatedFiles::new();
        let mut have = Vec::new();
        have.resize_with(num_pieces, || AtomicBool::new(false));
        let mut check_files = false;
        for file in &torrent.metainfo.files {
            let path = &file.path;
            if path.exists() && std::fs::metadata(path).unwrap().len() == file.length as u64 {
                let f = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)?;
                check_files = true;
                allocated_files.insert(path.to_path_buf(), f);
            } else {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let f = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .open(&path)?;
                f.set_len(file.length as u64)?;
                allocated_files.insert(path.to_path_buf(), f);
            }
        }
        let mut last_piece_length = torrent.metainfo.total_size % torrent.metainfo.piece_length;
        if last_piece_length == 0 {
            last_piece_length = torrent.metainfo.piece_length;
        }
        let info = Arc::new(FileSystemInfo {
            piece_hashes: torrent.metainfo.pieces.clone(),
            files_info: torrent.metainfo.files.clone(),
            piece_length: torrent.metainfo.piece_length,
            last_piece_length,
            files: allocated_files.into(),
            have: have.into(),
            have_count: Default::default(),
        });
        let fs = FileSystem {
            failed_hash,
            have_send,
            info,
            piece_info,
            snapshot_blocks_received: 0,
            sender: None,
            write_thread: None,
            write_cache: HashMap::new(),
        };
        if !check_files {
            return Ok(fs);
        }
        let mut have_count = 0;
        for i in 0..num_pieces {
            let piece = fs.get_bytes(i, 0, fs.get_piece_length(i));
            if !is_valid_piece(&piece, i, &fs.info.piece_hashes) {
                continue;
            }
            have_count += 1;
            fs.info.have[i].store(true, Ordering::Relaxed);
        }
        fs.info.have_count.store(have_count, Ordering::Relaxed);
        Ok(fs)
    }

    fn done(&self) -> bool {
        return self.info.have_count.load(Ordering::Relaxed) == self.info.piece_hashes.len();
    }

    // This should have a cache layer so we don't go to disk for each block
    fn get_block(&self, index: usize, offset: usize) -> Option<Vec<u8>> {
        if !self.info.have[index].load(Ordering::Relaxed) {
            return None;
        }
        Some(self.get_bytes(index, offset, BLOCK_LENGTH))
    }

    // Returns None if we do not have that piece
    // This should have a cache layer so we don't go to disk everytime
    fn get_piece(&self, index: usize) -> Option<Vec<u8>> {
        if !self.info.have[index].load(Ordering::Relaxed) {
            return None;
        }
        Some(self.get_bytes(index, 0, self.get_piece_length(index)))
    }
    fn get_block_have(&self, index: usize) -> BitVec {
        let num_blocks = self.piece_info.get_num_blocks(index);
        if self.info.have[index].load(Ordering::Relaxed) {
            return BitVec::from_elem(num_blocks, true);
        }
        if let Some(value) = self.write_cache.get(&index) {
            value.have.clone()
        } else {
            BitVec::from_elem(num_blocks, false)
        }
    }

    fn have(&self) -> BitVec {
        BitVec::from_fn(self.num_pieces(), |x| {
            self.info.have[x].load(Ordering::Relaxed)
        })
    }

    fn num_pieces(&self) -> usize {
        self.info.piece_hashes.len()
    }

    fn percent_done(&self) -> f64 {
        return self.info.have_count.load(Ordering::Relaxed) as f64 * 100.0
            / self.info.piece_hashes.len() as f64;
    }

    fn verify_all_files(&self) -> bool {
        let num_pieces = self.info.piece_hashes.len();
        let mut result = true;
        for i in 0..num_pieces {
            let piece = self.get_piece(i).unwrap();
            result = is_valid_piece(&piece, i, &self.info.piece_hashes);
            if !result {
                warn!("Hash check failed on piece {}", i);
            }
        }
        result
    }

    fn write(&mut self, piece: CompletedPiece) -> Result<(), std::io::Error> {
        match &self.sender {
            Some(sender) => sender.send(piece).unwrap(),
            None => {
                let (sender, receiver) = channel();
                sender.send(piece).unwrap();
                self.sender = Some(sender);
                let info = self.info.clone();
                let mut failed_hash_sender = self.failed_hash.clone();
                let mut have_send = self.have_send.clone();
                self.write_thread = Some(thread::spawn(move || {
                    while let Ok(piece) = receiver.recv() {
                        info.write(&mut failed_hash_sender, &mut have_send, piece);
                    }
                }))
            }
        }
        Ok(())
    }
}

// Not the best name, but I need somewhere to collect the pieces that will be shared between threads
// Should add any write errors here to be picked up later (since writes are asynchronous)
pub struct FileSystemInfo {
    piece_hashes: Vec<Sha1Hash>,
    files_info: Vec<File>,
    piece_length: usize,
    last_piece_length: usize,
    files: Mutex<AllocatedFiles>,
    have_count: AtomicUsize,
    pub have: Vec<AtomicBool>,
}

impl FileSystemInfo {
    fn write(
        &self,
        failed_hash: &mut Sender<(usize, usize)>,
        have_send: &mut Sender<Have>,
        piece: CompletedPiece,
    ) {
        if self.have[piece.index].load(Ordering::Relaxed) {
            return;
        }
        debug!("Writing piece {} to disk", piece.index);
        if !is_valid_piece(&piece.piece, piece.index, &self.piece_hashes) {
            warn!("Piece {} has invalid hash", piece.index);
            failed_hash.send((piece.index, piece.piece.len())).unwrap();
            return;
        }
        let piece_begin_byte = piece.index * self.piece_length;
        let mut piece_current_byte = 0;
        let mut file_begin = 0;
        for file_info in self.files_info.iter() {
            let file_end = file_begin + file_info.length;
            if piece_begin_byte >= file_end {
                file_begin = file_end;
                continue;
            }
            let mut file_temp = self.files.lock().unwrap();
            let file = file_temp.get_mut(file_info.path.as_path()).unwrap();
            let piece_start_byte_in_file = if piece_begin_byte < file_begin {
                0
            } else {
                piece_begin_byte - file_begin
            };
            file.seek(SeekFrom::Start(piece_start_byte_in_file as u64))
                .unwrap();
            let num_bytes_to_write = std::cmp::min(
                piece.piece.len() - piece_current_byte,
                file_end - piece_start_byte_in_file,
            );
            let start = piece_current_byte;
            let end = start + num_bytes_to_write;
            file.write_all(&piece.piece[start..end]).unwrap();
            if end == piece.piece.len() {
                file.flush().unwrap();
                self.have[piece.index].store(true, Ordering::Relaxed);
                self.have_count.fetch_add(1, Ordering::Relaxed);
                have_send.send(Have { index: piece.index }).unwrap();
                return;
            }
            piece_current_byte = end;
            file_begin = file_end;
        }
        panic!("I don't think it should get here");
    }
}

fn is_valid_piece(piece: &[u8], index: usize, piece_hashes: &Vec<Sha1Hash>) -> bool {
    let actual = hash_to_bytes(piece);
    let expected = piece_hashes[index];
    actual == expected
}

#[derive(Debug)]
pub struct CompletedPiece {
    pub index: usize,
    pub piece: Vec<u8>,
}

#[derive(Debug)]
pub struct PieceInFlight {
    pub index: usize,
    piece: Vec<u8>,
    pub have: BitVec,
    piece_info: PieceInfo,
    blocks_received: usize,
}

impl PieceInFlight {
    pub fn new(index: usize, piece_info: PieceInfo) -> PieceInFlight {
        let length = piece_info.get_piece_length(index);
        let mut piece = Vec::new();
        piece.reserve(length);
        unsafe { piece.set_len(length) }
        let num_blocks = piece_info.get_num_blocks(index);
        let have = BitVec::from_elem(num_blocks, false);
        PieceInFlight {
            index,
            piece,
            have,
            piece_info,
            blocks_received: 0,
        }
    }

    pub fn add_block<T: Read>(
        &mut self,
        mut block: BlockReader<T>,
    ) -> Result<Option<CompletedPiece>, std::io::Error> {
        debug_assert!(block.piece_index() == self.index);
        debug_assert!(self.piece.len() != 0);
        if block.block_index() >= self.have.len() {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        if self.have[block.block_index()] {
            return Ok(None);
        }
        let block_length = self
            .piece_info
            .get_block_length(block.block_index(), self.index);
        if block_length != block.len() {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        self.blocks_received += 1;
        let end = block.begin() + block.len();
        block.read(&mut self.piece[block.begin()..end])?;
        self.have.set(block.block_index(), true);
        if self.blocks_received == self.have.len() {
            debug!("Got piece {}", self.index);
            Ok(Some(CompletedPiece {
                index: self.index,
                piece: std::mem::replace(&mut self.piece, Vec::new()),
            }))
        } else {
            Ok(None)
        }
    }
}
