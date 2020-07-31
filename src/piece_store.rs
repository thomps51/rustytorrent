use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;

use bit_vec::BitVec;
use memmap::MmapMut;

use crate::block_manager::CompletedPiece;
use crate::hash;
use crate::hash::Sha1Hash;
use crate::torrent::Torrent;

// Struct that represents a "store" that writes pieces to file and has the current state of the
// download
// Maybe can have different "stores", the main one writes to the file system but you can also
// write to network drive, ftp (which would involve more caching in memory to avoid transit delays), etc.

pub trait PieceStore: Sized {
    fn new(torrent: &Torrent) -> Result<Self, Box<dyn Error>>;

    fn get(&self, index: usize) -> Option<Vec<u8>>;

    fn have(&self) -> &BitVec;

    fn write(&mut self, piece: CompletedPiece) -> Result<(), std::io::Error>;
}

pub type AllocatedFiles = HashMap<PathBuf, MmapMut>;

fn validate(piece: &CompletedPiece, piece_hashes: &Vec<Sha1Hash>) {
    let actual = hash::hash_to_bytes(&piece.piece);
    let expected = piece_hashes[piece.index];
    assert_eq!(actual, expected);
}

pub struct FileSystemPieceStore {
    piece_hashes: Vec<Sha1Hash>,
    files_info: Vec<crate::meta_info::File>,
    piece_length: usize,
    files: AllocatedFiles,
    have: BitVec,
}

impl PieceStore for FileSystemPieceStore {
    fn new(torrent: &Torrent) -> Result<Self, Box<dyn Error>> {
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
            let mmap = unsafe { MmapMut::map_mut(&f)? };
            allocated_files.insert(path.to_path_buf(), mmap);
        }
        let num_pieces = torrent.metainfo.pieces.len();
        Ok(Self {
            piece_hashes: torrent.metainfo.pieces.clone(),
            files_info: torrent.metainfo.files.clone(),
            piece_length: torrent.metainfo.piece_length as usize,
            files: allocated_files,
            have: BitVec::from_elem(num_pieces, false),
        })
    }

    // Returns None if we do not have that piece
    fn get(&self, index: usize) -> Option<Vec<u8>> {
        let value = self.have.get(index);
        if !value.unwrap_or(false) {
            return None;
        }
        let mut result = Vec::new();
        result.reserve(self.piece_length);
        let piece_begin_byte = index * self.piece_length;
        let mut file_begin = 0;
        for file_info in &self.files_info {
            let file_end = file_begin + file_info.length;
            if piece_begin_byte >= file_end {
                file_begin = file_end;
                continue;
            }
            let file = self
                .files
                .get(file_info.path.as_path())
                .expect("Logic error");
            let mut file_current_byte = 0;
            let piece_start_byte_in_file = if piece_begin_byte < file_begin {
                0
            } else {
                piece_begin_byte - file_begin
            };
            while file_current_byte < file_info.length {
                let file_index = piece_start_byte_in_file + file_current_byte;
                if file_index >= file.len() {
                    break; // on next  iteration of for loop, I no longer care about the piece begin
                }
                result.push(file[file_index]);
                file_current_byte += 1;
                if result.len() == self.piece_length {
                    return Some(result);
                }
            }
            file_begin = file_end;
        }
        Some(result)
    }

    fn have(&self) -> &BitVec {
        return &self.have;
    }

    // likely should be async
    fn write(&mut self, piece: CompletedPiece) -> Result<(), std::io::Error> {
        validate(&piece, &self.piece_hashes);
        self.have.set(piece.index, true);
        let piece_begin_byte = piece.index * self.piece_length;
        let mut piece_current_byte = 0;
        let mut file_begin = 0;
        for file_info in &self.files_info {
            let file_end = file_begin + file_info.length;
            if piece_begin_byte >= file_end {
                file_begin = file_end;
                continue;
            }
            let file = self
                .files
                .get_mut(file_info.path.as_path())
                .expect("Logic error");
            let mut file_current_byte = 0;
            let piece_start_byte_in_file = if piece_begin_byte < file_begin {
                0
            } else {
                piece_begin_byte - file_begin
            };
            while file_current_byte < file_info.length {
                let index = piece_start_byte_in_file + file_current_byte;
                if index >= file.len() {
                    break; // on next  iteration of for loop, I no longer care about the piece begin
                }
                file[index] = piece.piece[piece_current_byte];
                file_current_byte += 1;
                piece_current_byte += 1;
                if piece_current_byte == piece.piece.len() {
                    file.flush()?;
                    return Ok(());
                }
            }
            file_begin = file_end;
        }
        Ok(())
    }
}
