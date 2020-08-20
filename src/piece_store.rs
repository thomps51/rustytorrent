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

use crate::block_manager::CompletedPiece;
use crate::block_manager::PieceInFlight;
use crate::hash;
use crate::hash::Sha1Hash;
use crate::messages::Block;
use crate::meta_info::File;
use crate::torrent::Torrent;
use crate::SharedPieceAssigner;

// Struct that represents a "store" that writes pieces to file and has the current state of the
// download
// Maybe can have different "stores", the main one writes to the file system but you can also
// write to network drive, ftp (which would involve more caching in memory to avoid transit delays), etc.

pub trait PieceStore: Sized {
    // Create a new PieceStore for torrent.
    fn new(torrent: &Torrent, piece_assigner: SharedPieceAssigner) -> Result<Self, Box<dyn Error>>;

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

    fn num_pieces(&self) -> usize;

    fn percent_done(&self) -> f64;

    fn done(&self) -> bool;

    fn write_block(&mut self, block: Block) -> Result<(), std::io::Error>;

    fn write(&mut self, piece: CompletedPiece) -> Result<(), std::io::Error>;
}

pub type AllocatedFiles = HashMap<PathBuf, fs::File>;

pub struct FileSystem {
    info: Arc<FileSystemInfo>,
    sender: Option<Sender<CompletedPiece>>,
    write_cache: HashMap<usize, PieceInFlight>,
    write_thread: Option<JoinHandle<()>>,
    piece_assigner: SharedPieceAssigner,
}

impl FileSystem {
    fn get_bytes(&self, index: usize, offset: usize, num_bytes: usize) -> Option<Vec<u8>> {
        if self.info.have[index].load(Ordering::Relaxed) {
            return None;
        }
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
                return Some(result);
            }
            file_begin = file_end;
        }
        result.resize(current_index, 0);
        Some(result)
    }

    fn get_piece_length(&self, index: usize) -> usize {
        if index == self.info.piece_hashes.len() - 1 {
            self.info.last_piece_length
        } else {
            self.info.piece_length
        }
    }
}

impl PieceStore for FileSystem {
    fn new(torrent: &Torrent, piece_assigner: SharedPieceAssigner) -> Result<Self, Box<dyn Error>> {
        let mut allocated_files = AllocatedFiles::new();
        for file in &torrent.metainfo.files {
            let path = &file.path;

            // check if file exists
            // check if file is the correct size
            // if it's the correct size, determine which pieces are valid and which need to still be downloaded
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // currently just recreates files
            let f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(&path)?;
            f.set_len(file.length as u64)?;
            allocated_files.insert(path.to_path_buf(), f);
        }
        let num_pieces = torrent.metainfo.pieces.len();
        //let have = BitVec::from_elem(num_pieces, false);
        let mut have = Vec::new();
        have.resize_with(num_pieces, || AtomicBool::new(false));
        let info = Arc::new(FileSystemInfo {
            piece_hashes: torrent.metainfo.pieces.clone(),
            files_info: torrent.metainfo.files.clone(),
            piece_length: torrent.metainfo.piece_length,
            last_piece_length: torrent.metainfo.total_size % torrent.metainfo.piece_length,
            files: allocated_files.into(),
            have: have.into(),
            have_count: Default::default(),
        });
        Ok(FileSystem {
            info,
            sender: None,
            write_thread: None,
            write_cache: HashMap::new(),
            piece_assigner: piece_assigner,
        })
    }

    fn done(&self) -> bool {
        return self.info.have_count.load(Ordering::Relaxed) == self.info.piece_hashes.len();
    }

    // This should have a cache layer so we don't go to disk everytime
    fn get_block(&self, index: usize, offset: usize) -> Option<Vec<u8>> {
        const BLOCK_SIZE: usize = 1 << 14; // 16 KiB
        self.get_bytes(index, offset, BLOCK_SIZE)
    }

    // Returns None if we do not have that piece
    // This should have a cache layer so we don't go to disk everytime
    fn get_piece(&self, index: usize) -> Option<Vec<u8>> {
        self.get_bytes(index, 0, self.info.piece_length)
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

    fn write(&mut self, piece: CompletedPiece) -> Result<(), std::io::Error> {
        match &self.sender {
            Some(sender) => {
                if let Err(error) = sender.send(piece) {
                    println!("{}", error);
                    panic!(error);
                }
            }
            None => {
                let (sender, receiver) = channel();
                sender.send(piece).unwrap();
                self.sender = Some(sender);
                let info = self.info.clone();
                self.write_thread = Some(thread::spawn(move || {
                    while let Ok(piece) = receiver.recv() {
                        info.write(piece);
                    }
                }))
            }
        }
        Ok(())
    }

    fn write_block(&mut self, block: Block) -> Result<(), std::io::Error> {
        let piece_length = self.get_piece_length(block.index);
        let piece = self
            .write_cache
            .entry(block.index)
            .or_insert_with(|| PieceInFlight::new(piece_length, block.index));
        if let Some(completed) = piece.add_block(&block) {
            self.write(completed)?;
            self.write_cache.remove(&block.index);
        }
        Ok(())
    }
}

// Not the best name, but I need somewhere to collect the pieces that will be shared between threads
// Should add any write errors here to be picked up later (since writes are asynchronous)
struct FileSystemInfo {
    piece_hashes: Vec<Sha1Hash>,
    files_info: Vec<File>,
    piece_length: usize,
    last_piece_length: usize,
    files: Mutex<AllocatedFiles>,
    have_count: AtomicUsize,
    have: Vec<AtomicBool>,
}

impl FileSystemInfo {
    fn write(&self, piece: CompletedPiece) {
        if self.have[piece.index].load(Ordering::Relaxed) {
            return;
        }
        if !is_valid_piece(&piece, &self.piece_hashes) {
            warn!("Piece {} has invalid hash", piece.index);
            return; // PieceAssigner will take care of assigning missing pieces in Endgame mode (only if there are other peers though...)
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
                return;
            }
            piece_current_byte = end;
            file_begin = file_end;
        }
        panic!("I don't think it should get here");
    }
}

fn is_valid_piece(piece: &CompletedPiece, piece_hashes: &Vec<Sha1Hash>) -> bool {
    let actual = hash::hash_to_bytes(&piece.piece);
    let expected = piece_hashes[piece.index];
    actual == expected
}
