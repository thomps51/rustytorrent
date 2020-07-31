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
const MAX_OPEN_REQUESTS_PER_PEER: usize = 10;
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

fn handle_client(
   mut stream: TcpStream,
   torrent: &Torrent,
   piece_assigner: SharedPieceAssigner,
   piece_store: SharedPieceStore,
) {
   let num_pieces = torrent.metainfo.pieces.len();
   let handshake_from_peer = messages::Handshake::read_from(&mut stream).unwrap();
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
   loop {
      if let UpdateSuccess::NoUpdate = peer.update().unwrap() {
         std::time::Duration::from_millis(10);
      }
      if piece_store.borrow_mut().have().all() {
         break;
      }
   }
   validate_all_files(&torrent);
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
   let piece_assigner = Rc::new(RefCell::new(PieceAssigner::new(
      num_pieces,
      torrent.metainfo.total_size,
      piece_length,
   )));
   let piece_store: Rc<RefCell<_>> =
      Rc::new(RefCell::new(FileSystemPieceStore::new(&torrent).unwrap()));
   for stream in listener.incoming() {
      handle_client(
         stream?,
         &torrent,
         piece_assigner.clone(),
         piece_store.clone(),
      );
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
