// Goal: CLI that, when given a torrent file, will download it to completion

use std::cell::RefCell;
use std::error::Error;
use std::path::Path;
use std::rc::Rc;

use log::info;
use slog::Drain;

mod bencoding;
mod block_manager;
mod connection;
mod hash;
//mod lru_cache;
mod connection_manager;
mod messages;
mod meta_info;
mod piece_assigner;
mod piece_store;
mod torrent;
mod tracker;

use piece_assigner::PieceAssigner;
use piece_store::*;
use torrent::Torrent;

#[macro_use(slog_o)]
extern crate slog;

// Stats for Vuze
// Value : Max Speed (MB/s)
// 10    : 2 MB/s
// 20    : 4 MB/s
// 50    : 12 MB/s
// 75    : 18 MB/s
// 100   : 18 MB/s
// 200   : 18 MB/s
//
// Stats for qBittorrent:
// 15    : 100 MiB/s
//
// I likely want a dynamic value based on if the peer completed it's requested pieces within a certain amount of time.
const MAX_OPEN_REQUESTS_PER_PEER: usize = 15;
const PEER_ID: &'static str = "-QQ00010000000000000";

type SharedPieceAssigner = Rc<RefCell<PieceAssigner>>;
type SharedPieceStore = Rc<RefCell<FileSystem>>; // impl Trait syntax pls

// validate files (since we are validating all pieces as they come in, this is mostly a sanity check)
// needs to not load all of file into memory
fn validate_all_files(torrent: &Torrent) {
   let num_pieces = torrent.metainfo.pieces.len();
   let mut file_iter = torrent.metainfo.files.iter();
   let mut current_file =
      std::fs::read(&file_iter.next().expect("Always at least one file").path).unwrap();
   let mut current_file_index = 0;
   for i in 0..num_pieces {
      let mut current_piece = Vec::new();
      let piece_length = if i == num_pieces - 1 {
         let leftover = torrent.metainfo.total_size % torrent.metainfo.piece_length;
         if leftover == 0 {
            torrent.metainfo.piece_length
         } else {
            leftover
         }
      } else {
         torrent.metainfo.piece_length
      };
      while current_piece.len() < piece_length {
         let start = current_file_index;
         let remaining = piece_length - current_piece.len();
         let available_from_file = current_file.len() - current_file_index;
         if available_from_file >= remaining {
            let end = current_file_index + remaining;
            current_piece.extend_from_slice(&current_file[start..end]);
            current_file_index += remaining;
         } else {
            let end = current_file_index + available_from_file;
            current_piece.extend_from_slice(&current_file[start..end]);
            if let Some(file) = file_iter.next() {
               current_file_index = 0;
               current_file = std::fs::read(&file.path).unwrap();
            }
         }
      }
      let actual = hash::hash_to_bytes(&current_piece);
      let expected = torrent.metainfo.pieces[i];
      assert_eq!(actual, expected); // Since we have verified the individual pieces, something has
                                    // gone quite wrong if this fails
   }
}

fn main() -> Result<(), Box<dyn Error>> {
   let log_path = "rusty.log";
   let file = std::fs::OpenOptions::new()
      .create(true)
      .write(true)
      .truncate(true)
      .open(log_path)
      .unwrap();

   let decorator = slog_term::PlainDecorator::new(file);
   let drain = slog_term::FullFormat::new(decorator).build().fuse();
   let drain = slog_async::Async::new(drain).build().fuse();
   let logger = slog::Logger::root(drain, slog_o!());
   let _scope_guard = slog_scope::set_global_logger(logger);
   let _log_guard = slog_stdlog::init().unwrap();

   let args: Vec<String> = std::env::args().collect();
   // TODO validate args
   info!("Torrent file: {}", args[1]);
   let torrent = Torrent::from_file(Path::new(&args[1]))?;

   let mut manager = connection_manager::ConnectionManager::new(torrent);
   manager.run()?;

   //validate_all_files(&torrent);
   info!("done!");
   Ok(())
}
