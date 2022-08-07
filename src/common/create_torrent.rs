use crate::bencoding::*;
use std::error::Error;
use std::path::Path;
use std::{
    fs::File,
    io::{BufRead, BufReader},
};

use super::hash_to_bytes;

pub fn create_torrent_metadata_from_path(
    path: &Path,
    tracker: &str,
    piece_length: usize,
) -> Result<Dictionary, Box<dyn Error>> {
    // Assume file for now: TODO handle directories as well
    let mut result = Dictionary::new();
    result.insert("announce".to_owned(), tracker.to_owned());
    let mut info = Dictionary::new();
    let file_name = path.file_name().unwrap().to_owned().into_string().unwrap();
    info.insert("name".to_owned(), file_name);
    info.insert("piece length".to_owned(), piece_length);
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(piece_length, file);

    let mut pieces = Data::new();
    let mut file_length = 0;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        let hash = hash_to_bytes(&buffer);
        pieces.extend(hash);
        let length = buffer.len();
        file_length += length;
        reader.consume(length);
    }
    log::info!(
        "Created torrent with file_length: {}, piece_length: {}, num_pieces: {}",
        file_length,
        piece_length,
        pieces.len()
    );
    info.insert("pieces".to_owned(), pieces);
    info.insert("length".to_owned(), file_length);
    result.insert("info".to_owned(), info);
    Ok(result)
}

pub fn create_torrent_from_path(
    path: &Path,
    tracker: &str,
    piece_length: usize,
    destination: &Path,
) -> Result<(), Box<dyn Error>> {
    let data = create_torrent_metadata_from_path(path, tracker, piece_length)?;
    let mut file = File::create(destination)?;
    data.bencode_to(&mut file)?;
    Ok(())
}
