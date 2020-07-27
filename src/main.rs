// Goal: CLI that, when given a torrent file, will download it to completion

use std::cell::RefCell;
use std::error::Error;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::rc::Rc;

//use mio::net::{TcpListener, TcpStream};
//use mio::{Events, Interest, Poll, Token};

mod bencoding;
mod block_manager;
mod connection;
mod hash;
mod message;
mod meta_info;
mod piece_assigner;
mod piece_store;
mod torrent;
mod tracker;

use connection::*;
use message::*;
use piece_assigner::PieceAssigner;
use piece_store::*;
use torrent::Torrent;
use tracker::EventKind;
use tracker::Tracker;

const LISTEN_PORT: u16 = 6881;
const MAX_OPEN_REQUESTS_PER_PEER: usize = 10;
const PEER_ID: &'static str = "-QQ00010000000000000";

const TEST_MODE: bool = false;

fn handle_client(mut stream: TcpStream, torrent: &Torrent) {
   let num_pieces = torrent.metainfo.pieces.len();
   let piece_length = torrent.metainfo.piece_length;
   // These will be for each client
   let piece_assigner: Rc<RefCell<_>> = Rc::new(RefCell::new(PieceAssigner::new(
      num_pieces,
      torrent.metainfo.total_size,
      piece_length,
   )));
   let piece_store: Rc<RefCell<_>> =
      Rc::new(RefCell::new(FileSystemPieceStore::new(&torrent).unwrap()));

   let handshake_from_peer = message::Handshake::read_from(&mut stream).unwrap();
   println!("Received handshake: {:?}", handshake_from_peer);
   let mut peer = Connection::new_from_stream(
      num_pieces,
      &handshake_from_peer.peer_id,
      stream,
      piece_assigner.clone(),
      piece_store.clone(),
   );
   let handshake_to_peer = message::Handshake::new(PEER_ID, &torrent.metainfo.info_hash_raw);
   handshake_to_peer.write_to(&mut peer.stream).unwrap();

   let msg = message::Unchoke {};
   msg.write_to(&mut peer.stream).unwrap();

   let msg = message::Interested {};
   msg.write_to(&mut peer.stream).unwrap();

   loop {
      if let UpdateSuccess::NoUpdate = peer.update().unwrap() {
         std::time::Duration::from_millis(10);
      }
      if piece_store.borrow_mut().have().all() {
         break;
      }
   }

   // validate file
   let file_raw_bytes = std::fs::read(&torrent.metainfo.files[0].path).unwrap();
   for i in 0..num_pieces {
      let start = i * torrent.metainfo.piece_length;
      let end = start
         + if i != num_pieces - 1 {
            torrent.metainfo.piece_length
         } else {
            torrent.metainfo.total_size % torrent.metainfo.piece_length
         };
      let actual = hash::hash_to_bytes(&file_raw_bytes[start..end]);
      println!("File raw bytes: {:?}", &file_raw_bytes[start..end]);
      let expected = torrent.metainfo.pieces[i];
      println!("Validating piece {}", i);
      assert_eq!(actual, expected);
   }
}

fn main() -> Result<(), Box<dyn Error>> {
   let args: Vec<String> = std::env::args().collect();
   // TODO validate args
   println!("Torrent file: {}", args[1]);
   let torrent = Torrent::from_file(Path::new(&args[1]))?;
   let tracker = Tracker {
      address: torrent.metainfo.announce.clone(),
   };
   //let files = allocate_files(&torrent)?;
   let response = tracker.announce(&torrent, EventKind::Started)?;
   println!("tracker response: {:?}", response);

   let mut current_peer_index = 0;

   let num_pieces = torrent.metainfo.pieces.len();
   let piece_length = torrent.metainfo.piece_length;

   let listener = TcpListener::bind(format!("10.0.0.2:{}", LISTEN_PORT).as_str())?;

   println!("Total file size: {}", torrent.metainfo.total_size);
   println!("Piece size: {}", torrent.metainfo.piece_length);
   println!(
      "Last Piece size: {}",
      torrent.metainfo.total_size % torrent.metainfo.piece_length
   );

   println!("Listening for connection on port {}", LISTEN_PORT);
   for stream in listener.incoming() {
      handle_client(stream?, &torrent);
      break; // only expect one for now
   }

   /*
   let mut peer = Connection::new(
      num_pieces,
      piece_length,
      piece_index,
      response.peer_list[current_peer_index].clone(),
   )?;
   println!("Successfully connected to peer");
   let handshake_to_peer = message::Handshake::new(PEER_ID, &torrent.metainfo.info_hash_raw);
   handshake_to_peer.write_to(&mut peer.stream)?;

   //let handshake_from_peer = message::Handshake::read_from(&mut peer.stream)?;
   //println!("Received handshake: {:?}", handshake_from_peer);

   let mut have_pieces = bit_vec::BitVec::from_elem(num_pieces as usize, false);
   for piece_index in 0..num_pieces {
      println!("Attempting to download piece {}", piece_index);
      loop {
         let debug = peer.update(&mut have_pieces);
         println!("Update result: {:?}", debug);
         /*
         match peer.update(&mut have_pieces) {
            UpdateSuccess::NoUpdate => ,
            UpdateSuccess::PieceComplete => ,
            UpdateSuccess::Success =>,

         }
         */
         std::thread::sleep(std::time::Duration::from_secs(1));
         // If update completed a piece, add it to the file
      }
   }
   */
   // Need to have listening thread that spawns
   Ok(())
}
