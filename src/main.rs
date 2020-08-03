// Goal: CLI that, when given a torrent file, will download it to completion

use std::cell::RefCell;
use std::error::Error;
use std::io::prelude::*;
use std::path::Path;
use std::rc::Rc;

use mio::net::TcpStream;
use mio::{Events, Interest, Poll, Token};
//use mio::net::{TcpListener, TcpStream};
//use mio::{Events, Interest, Poll, Token};

mod bencoding;
mod block_manager;
mod connection;
mod hash;
mod messages;
mod meta_info;
mod piece_assigner;
mod piece_store;
mod torrent;
mod tracker;

use connection::*;
use messages::*;
use piece_assigner::PieceAssigner;
use piece_store::*;
use torrent::Torrent;
use tracker::EventKind;
use tracker::Tracker;

const LISTEN_PORT: u16 = 6881;

// Value : Max Speed (MB/s)
// 10    : 2 MB/s
// 20    : 4 MB/s
// 100   : 14 MB/s
//
// I likely want a dynamic value based on if the peer completed it's requested pieces within a certain amount of time.
const MAX_OPEN_REQUESTS_PER_PEER: usize = 200;
const PEER_ID: &'static str = "-QQ00010000000000000";

const TEST_MODE: bool = false;

type SharedPieceAssigner = Rc<RefCell<PieceAssigner>>;
type SharedPieceStore = Rc<RefCell<FileSystemPieceStore>>; // impl Trait syntax pls

// validate files (since we are validating all pieces as they come in, this is mostly a sanity check)
fn validate_all_files(torrent: &Torrent) {
   let num_pieces = torrent.metainfo.pieces.len();
   let mut file_iter = torrent.metainfo.files.iter();
   let mut current_file =
      std::fs::read(&file_iter.next().expect("Always at least one file").path).unwrap();
   let mut current_file_index = 0;
   for i in 0..num_pieces {
      let mut current_piece = Vec::new();
      let piece_length = if i != num_pieces - 1 {
         torrent.metainfo.piece_length
      } else {
         torrent.metainfo.total_size % torrent.metainfo.piece_length
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
      assert_eq!(actual, expected);
   }
}

fn create_connection(
   mut stream: TcpStream,
   torrent: &Torrent,
   piece_assigner: SharedPieceAssigner,
   piece_store: SharedPieceStore,
) -> Connection {
   println!("handling client");
   let num_pieces = torrent.metainfo.pieces.len();
   let mut buffer = Vec::new();
   buffer.resize(Handshake::SIZE as usize, 0);
   let mut total_read = 0;
   while total_read < Handshake::SIZE as usize {
      let read = match stream.read(&mut buffer[total_read..]) {
         Ok(l) => l,
         Err(error) => {
            if error.kind() == std::io::ErrorKind::WouldBlock {
               std::thread::sleep(std::time::Duration::from_millis(100));
               continue;
            }
            panic!(error);
         }
      };
      if read == 0 {
         panic!("EOF while reading handshake");
      }
      total_read += read;
   }
   let handshake_from_peer = messages::Handshake::read_from(&mut (&buffer[..])).unwrap();
   println!("Received handshake: {:?}", handshake_from_peer);
   let mut peer = Connection::new_from_stream(
      num_pieces,
      &handshake_from_peer.peer_id,
      stream,
      piece_assigner.clone(),
      piece_store.clone(),
   );
   let handshake_to_peer = messages::Handshake::new(PEER_ID, &torrent.metainfo.info_hash_raw);
   handshake_to_peer.write_to(&mut peer.stream).unwrap();
   let msg = Unchoke {};
   msg.write_to(&mut peer.stream).unwrap();
   let msg = Interested {};
   msg.write_to(&mut peer.stream).unwrap();
   println!("Wrote interested");
   peer
}

fn main() -> Result<(), Box<dyn Error>> {
   let args: Vec<String> = std::env::args().collect();
   // TODO validate args
   println!("Torrent file: {}", args[1]);
   let torrent = Torrent::from_file(Path::new(&args[1]))?;
   let tracker = Tracker {
      address: torrent.metainfo.announce.clone(),
   };
   let response = tracker.announce(&torrent, EventKind::Started)?;
   println!("tracker response: {:?}", response);

   let num_pieces = torrent.metainfo.pieces.len();
   let piece_length = torrent.metainfo.piece_length;

   println!("Total file size: {}", torrent.metainfo.total_size);
   println!("Piece size: {}", torrent.metainfo.piece_length);
   println!(
      "Last Piece size: {}",
      torrent.metainfo.total_size % torrent.metainfo.piece_length
   );

   println!("Listening for connection on port {}", LISTEN_PORT);
   let piece_assigner = Rc::new(RefCell::new(PieceAssigner::new(
      num_pieces,
      torrent.metainfo.total_size,
      piece_length,
   )));
   let piece_store: Rc<RefCell<_>> =
      Rc::new(RefCell::new(FileSystemPieceStore::new(&torrent).unwrap()));

   const LISTENER: Token = Token(std::usize::MAX);
   let mut connections = std::collections::HashMap::new();
   let mut next_socket_index = 0;
   let mut poll = Poll::new()?;
   let mut listener =
      mio::net::TcpListener::bind(format!("10.0.0.2:{}", LISTEN_PORT).as_str().parse()?)?;
   poll
      .registry()
      .register(&mut listener, LISTENER, Interest::READABLE)?;
   let mut events = Events::with_capacity(1024);
   loop {
      poll.poll(&mut events, None)?;
      for event in &events {
         match event.token() {
            LISTENER => loop {
               match listener.accept() {
                  Ok((socket, _)) => {
                     println!("new peer!");
                     let mut peer = create_connection(
                        socket,
                        &torrent,
                        piece_assigner.clone(),
                        piece_store.clone(),
                     );
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
               if let UpdateSuccess::NoUpdate =
                  connections.get_mut(&token).unwrap().update().unwrap()
               {
                  break;
               }
               if piece_store.borrow_mut().have().all() {
                  break;
               }
            },
         }
      }
      if piece_store.borrow_mut().have().all() {
         break;
      }
   }

   /*
   for stream in listener.incoming() {
      let stream = stream?;
      stream.set_nonblocking(true)?;
      handle_client(
         TcpStream::from_std(stream),
         &torrent,
         piece_assigner.clone(),
         piece_store.clone(),
      );
      break; // only expect one for now
   }
   */
   validate_all_files(&torrent);
   println!("done!");
   Ok(())
}
