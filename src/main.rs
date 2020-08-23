// Goal: CLI that, when given a torrent file, will download it to completion

use std::cell::RefCell;
use std::error::Error;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;

use log::{debug, info};
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token};
use slog::Drain;

mod bencoding;
mod block_manager;
mod connection;
mod hash;
//mod lru_cache;
mod messages;
mod meta_info;
mod piece_assigner;
mod piece_store;
mod torrent;
mod tracker;

use connection::*;
use piece_assigner::PieceAssigner;
use piece_store::*;
use torrent::Torrent;
use tracker::EventKind;
use tracker::Tracker;

#[macro_use(slog_o)]
extern crate slog;

const LISTEN_PORT: u16 = 6881;
const MAX_PEERS: usize = 30;

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

const TEST_MODE: bool = false;

type SharedPieceAssigner = Rc<RefCell<PieceAssigner>>;
type SharedPieceStore = Rc<RefCell<FileSystem>>; // impl Trait syntax pls

// validate files (since we are validating all pieces as they come in, this is mostly a sanity check)
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

fn create_connection(
   stream: TcpStream,
   torrent: &Torrent,
   piece_assigner: SharedPieceAssigner,
   piece_store: SharedPieceStore,
   id: usize,
) -> Result<Connection, std::io::Error> {
   info!("handling client {}", stream.peer_addr().unwrap());
   let num_pieces = torrent.metainfo.pieces.len();
   let peer = Connection::new_from_incoming(
      num_pieces,
      stream,
      piece_assigner.clone(),
      piece_store.clone(),
      torrent.metainfo.info_hash_raw,
      id,
   );
   Ok(peer)
}

fn connect_to_peer(
   peer: &tracker::PeerInfo,
   torrent: &Torrent,
   piece_assigner: SharedPieceAssigner,
   piece_store: SharedPieceStore,
   id: usize,
) -> Result<Connection, std::io::Error> {
   info!("connect_to_peer begin {} {}", id, peer.addr);
   let num_pieces = torrent.metainfo.pieces.len();
   let stream = TcpStream::connect(peer.addr)?;
   let peer = Connection::new_from_outgoing(
      peer.addr,
      num_pieces,
      stream,
      piece_assigner.clone(),
      piece_store.clone(),
      torrent.metainfo.info_hash_raw,
      id,
   );
   Ok(peer)
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
   let tracker = Tracker {
      address: torrent.metainfo.announce.clone(),
   };
   let mut response = tracker.announce(&torrent, EventKind::Started)?;
   debug!("tracker response: {:?}", response);

   let num_pieces = torrent.metainfo.pieces.len();
   let piece_length = torrent.metainfo.piece_length;

   debug!("Total file size: {}", torrent.metainfo.total_size);
   debug!("Piece size: {}", torrent.metainfo.piece_length);
   debug!("Num pieces: {}", torrent.metainfo.pieces.len());
   debug!(
      "Last Piece size: {}",
      torrent.metainfo.total_size % torrent.metainfo.piece_length
   );

   debug!("Listening for connection on port {}", LISTEN_PORT);

   let (send, recv) = mpsc::channel();
   let piece_assigner = Rc::new(RefCell::new(PieceAssigner::new(
      num_pieces,
      torrent.metainfo.total_size,
      piece_length,
      recv,
   )));
   let piece_store: Rc<RefCell<_>> =
      Rc::new(RefCell::new(FileSystem::new(&torrent, send).unwrap()));

   const LISTENER: Token = Token(std::usize::MAX);

   let mut connections = std::collections::HashMap::new();
   let mut next_socket_index = 0;
   let mut poll = Poll::new()?;
   let mut events = Events::with_capacity(1024);
   let mut listener = TcpListener::bind(format!("10.0.0.2:{}", LISTEN_PORT).as_str().parse()?)?;
   poll
      .registry()
      .register(&mut listener, LISTENER, Interest::READABLE)?;
   let num_peers = if response.peer_list.len() < MAX_PEERS {
      response.peer_list.len()
   } else {
      MAX_PEERS
   };
   let peer_infos = response.peer_list.drain(..num_peers);
   for peer_info in peer_infos {
      use std::str::FromStr;
      if peer_info.addr.ip() == std::net::IpAddr::from_str("10.0.0.2").unwrap() {
         continue;
      }
      let mut peer = match connect_to_peer(
         &peer_info,
         &torrent,
         piece_assigner.clone(),
         piece_store.clone(),
         next_socket_index,
      ) {
         Ok(value) => value,
         Err(err) => {
            debug!("Unable to connect to {}: {}", peer_info.addr, err);
            continue;
         }
      };
      let token = Token(next_socket_index);
      next_socket_index += 1;
      // Registering the socket for Writable notifications will tell us when it is connected.
      poll
         .registry()
         .register(&mut peer.stream, token, Interest::WRITABLE)?;
      connections.insert(token, peer);
   }
   // announce timer
   let mut last_update = std::time::Instant::now();
   let time_between_updates = std::time::Duration::from_secs(1);
   loop {
      poll.poll(&mut events, Some(std::time::Duration::from_secs(1)))?;
      let now = std::time::Instant::now();
      if now - last_update > time_between_updates {
         print!(
            "Percent done: {:.2}%\r",
            piece_store.borrow().percent_done()
         );
         use std::io::prelude::*;
         std::io::stdout()
            .flush()
            .ok()
            .expect("Could not flush stdout");
         last_update = now;
      }
      for event in &events {
         match event.token() {
            LISTENER => loop {
               match listener.accept() {
                  Ok((socket, _)) => {
                     // handle_new_peer
                     println!("new peer!");
                     let mut peer = match create_connection(
                        socket,
                        &torrent,
                        piece_assigner.clone(),
                        piece_store.clone(),
                        next_socket_index,
                     ) {
                        Ok(conn) => conn,
                        Err(err) => {
                           info!("Handshaked failed: {}", err);
                           continue;
                        }
                     };
                     let token = Token(next_socket_index);
                     next_socket_index += 1;
                     poll
                        .registry()
                        .register(&mut peer.stream, token, Interest::READABLE)?;
                     connections.insert(token, peer);
                  }
                  Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                     break;
                  }
                  e => panic!("err={:?}", e),
               }
            },
            token => loop {
               let peer = connections.get_mut(&token).unwrap();
               if event.is_writable() {
                  // Writable means it is now connected
                  poll
                     .registry()
                     .reregister(&mut peer.stream, token, Interest::READABLE)?;
               }
               match peer.update() {
                  Ok(status) => {
                     if let UpdateSuccess::NoUpdate = status {
                        break;
                     }
                  }
                  Err(error) => {
                     // error in connection, remove
                     info!("Removing peer {}: {}", token.0, error);
                     poll.registry().deregister(&mut peer.stream)?;
                     connections.remove(&token);
                     break;
                  }
               }
               if piece_store.borrow().done() {
                  break;
               }
            },
         }
      }
      if events.is_empty() {
         info!(
            "No new events, {}% done",
            piece_store.borrow().percent_done()
         );
      }
      if piece_store.borrow().done() {
         break;
      }
   }
   //validate_all_files(&torrent);
   info!("done!");
   Ok(())
}
