use std::error::Error;
use std::path::Path;
use std::path::PathBuf;

use crate::bencoding;
use crate::hash;
use bencoding::get_as;
use bencoding::Dictionary;
use bencoding::Encode;
use bencoding::List;
use hash::Sha1Hash;
use hash::SHA1_HASH_LENGTH;

#[derive(Debug, Clone)]
pub struct File {
    pub length: usize,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct MetaInfo {
    pub announce: String,
    pub piece_length: usize,
    pub pieces: Vec<Sha1Hash>,
    pub files: Vec<File>,
    pub total_size: usize,
    pub info_hash_raw: Sha1Hash,
    pub info_hash_uri: String,
}

impl MetaInfo {
    // Create MetaInfo from a file which has a bencoded dictionary (torrent file).
    pub fn from_file(file: &Path) -> Result<MetaInfo, Box<dyn Error>> {
        let file = std::fs::File::open(file)?;
        let mmap = unsafe { memmap::MmapOptions::new().map(&file)? };
        let result = bencoding::parse(&mmap).unwrap();
        if let bencoding::DataKind::Dictionary(value) = result {
            Ok(Self::from_dict(value)?)
        } else {
            Err(String::from("File needs to be a bencoded dictionary").into())
        }
    }

    pub fn from_dict(dict: Dictionary) -> Result<MetaInfo, Box<dyn Error>> {
        let announce: String = get_as(&dict, "announce")?;
        let info: Dictionary = get_as(&dict, "info")?;
        let piece_length: usize = get_as(&info, "piece length")?;
        let raw_pieces: Vec<u8> = get_as(&info, "pieces")?;
        assert_eq!(raw_pieces.len() % SHA1_HASH_LENGTH, 0);
        let num_pieces = raw_pieces.len() / SHA1_HASH_LENGTH;
        let mut pieces = Vec::new();
        pieces.resize(num_pieces, [0; 20]);
        for i in 0..num_pieces {
            let start = i * SHA1_HASH_LENGTH;
            let end = start + SHA1_HASH_LENGTH;
            pieces[i].copy_from_slice(&raw_pieces[start..end]);
        }
        let mut files = Vec::new();
        let mut total_size = 0;
        if info.contains_key("length") {
            // Single File Case
            let length = get_as(&info, "length")?;
            let path: String = get_as(&info, "name")?;
            files.push(File {
                length: length,
                path: path.into(),
            });
            total_size = length;
        } else {
            // Multi File Case
            let directory: String = get_as(&info, "name")?;
            let files_list: List = get_as(&info, "files")?;
            for file in files_list {
                let as_dict = file.as_dict()?;
                let length = get_as(as_dict, "length")?;
                total_size += length;
                let path_list: List = get_as(as_dict, "path")?;
                let mut path = PathBuf::new();
                path.push(&directory);
                for p in path_list {
                    path.push(p.as_utf8()?);
                }
                files.push(File { length, path });
            }
        }
        let bencoded_info = info.clone().bencode();
        let info_hash_uri = hash::hash_to_uri_str(&bencoded_info);
        let info_hash_raw = hash::hash_to_bytes(&bencoded_info);
        Ok(MetaInfo {
            announce,
            piece_length,
            pieces,
            files,
            info_hash_raw,
            info_hash_uri,
            total_size,
        })
    }
}
