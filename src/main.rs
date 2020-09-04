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
mod connection_manager;
mod hash;
mod messages;
mod meta_info;
mod piece_assigner;
mod piece_store;
//mod read_buffer;
mod torrent;
mod tracker;

use connection_manager::ConnectionManager;
use piece_assigner::PieceAssigner;
use piece_store::*;
use torrent::Torrent;

#[macro_use(slog_o)]
extern crate slog;

const PEER_ID: &'static str = "-QQ00010000000000000";

type SharedPieceAssigner = Rc<RefCell<PieceAssigner>>;
type SharedPieceStore = Rc<RefCell<FileSystem>>; // impl Trait syntax pls

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
   let mut manager = ConnectionManager::new(torrent);
   manager.run()?;
   info!("done!");
   Ok(())
}
