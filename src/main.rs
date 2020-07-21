// Goal: CLI that, when given a torrent file, will download it to completion

use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error;
use std::io::prelude::*;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;

use memmap::MmapMut;

mod bencoding;
mod connection;
mod hash;
mod message;
mod meta_info;
mod torrent;
mod tracker;

use connection::*;
use message::*;
use std::net::{TcpListener, TcpStream};
use torrent::Torrent;
use tracker::EventKind;
use tracker::Tracker;

const LISTEN_PORT: u16 = 6881;
const TEST_MODE: bool = false;
const PEER_ID: &'static str = "-QQ00010000000000000";

type PeerId = [u8; 20];

pub type AllocatedFiles = HashMap<PathBuf, MmapMut>;

// simple, monotonic count for now, should be "random", and be able to response if a peer does
// not have the piece that is assigned.

// Needs to also give size, since last piece will have a different size
pub struct PieceAssigner {
   counter: u32,
   total_pieces: u32,
   total_size: u64,
   piece_size: u32,
}

impl PieceAssigner {
   // index, size
   fn get(&mut self) -> (u32, u32) {
      let prev = self.counter;
      if prev >= self.total_pieces {
         panic!("requested too many pieces!");
      }
      let size = if prev == self.total_pieces - 1 {
         println!("Last piece: {}", prev);
         (self.total_size % self.piece_size as u64) as u32
      } else {
         self.piece_size
      };
      println!("PieceAssigner piece {} of size {}", prev, size);
      self.counter += 1;
      (prev, size)
   }

   fn has_pieces(&self) -> bool {
      self.counter != self.total_pieces
   }

   fn new(total_pieces: u32, total_size: u64, piece_size: u32) -> Self {
      Self {
         counter: 0,
         total_pieces,
         total_size,
         piece_size,
      }
   }
}

fn create_handshake(hashed_info: &hash::Sha1Hash, peer_id: &[u8]) -> Vec<u8> {
   debug_assert!(peer_id.len() == 20);
   let pstr = "BitTorrent protocol".as_bytes();
   let mut result = Vec::new();
   result.push(pstr.len() as u8);
   result.extend_from_slice(pstr);
   result.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
   result.extend_from_slice(hashed_info);
   result.extend_from_slice(peer_id);
   debug_assert!(result.len() == 49 + pstr.len());

   result
}

fn allocate_files(torrent: &Torrent) -> Result<AllocatedFiles, Box<dyn Error>> {
   let mut result = AllocatedFiles::new();
   for file in &torrent.metainfo.files {
      let path = &file.path;
      // check if file exists
      // check if file is the correct size
      let f = std::fs::OpenOptions::new()
         .read(true)
         .write(true)
         .create(true)
         .open(&path)?;
      f.set_len(file.length as u64)?;
      let mmap = unsafe { MmapMut::map_mut(&f)? };
      result.insert(path.to_path_buf(), mmap);
   }
   Ok(result)
}

fn handle_client(
   mut stream: TcpStream,
   num_pieces: u32,
   piece_length: u32,
   torrent: &Torrent,
   file_raw_bytes: &mut [u8],
) {
   let piece_assigner: Rc<RefCell<_>> = Rc::new(RefCell::new(PieceAssigner::new(
      num_pieces,
      torrent.metainfo.total_size as u64,
      piece_length,
   )));

   let handshake_from_peer = message::Handshake::read_from(&mut stream).unwrap();
   println!("Received handshake: {:?}", handshake_from_peer);
   let mut peer = Connection::new_from_stream(
      num_pieces,
      &handshake_from_peer.peer_id,
      stream,
      piece_assigner.clone(),
   );
   let handshake_to_peer = message::Handshake::new(PEER_ID, &torrent.metainfo.info_hash_raw);
   handshake_to_peer.write_to(&mut peer.stream).unwrap();

   let msg = message::Unchoke {};
   msg.write_to(&mut peer.stream).unwrap();

   let msg = message::Interested {};
   msg.write_to(&mut peer.stream).unwrap();

   let mut have_pieces = bit_vec::BitVec::from_elem(num_pieces as usize, false);
   loop {
      let debug = peer.update(&mut have_pieces).unwrap();
      //println!("Update result: {:?}", debug);
      match debug {
         UpdateSuccess::NoUpdate => std::thread::sleep(std::time::Duration::from_secs(1)),
         UpdateSuccess::PieceComplete(piece) => {
            //println!("Piece: {:?}", piece.piece);
            let actual = hash::hash_to_bytes(&piece.piece);
            let expected = torrent.metainfo.pieces[piece.index as usize];
            println!(
               "Piece {} with length {} complete!",
               piece.index,
               piece.piece.len(),
            );
            assert_eq!(actual, expected);

            let start = piece.index * piece_length;
            for i in 0..piece.piece.len() {
               let offset = start as usize + i;
               file_raw_bytes[offset] = piece.piece[i];
            }
            if have_pieces.all() {
               break;
            }
         }
         UpdateSuccess::Success => continue,
      }
      std::thread::sleep(std::time::Duration::from_millis(100));
      // If update completed a piece, add it to the file
   }
   // write file to disk;
   let path = &torrent.metainfo.files[0].path;
   let mut file = std::fs::OpenOptions::new()
      .read(true)
      .write(true)
      .create(true)
      .open(&path)
      .unwrap();
   println!("done! writing file to disk: {:?}", path);
   file.write_all(file_raw_bytes).unwrap();
}

fn main() -> Result<(), Box<dyn Error>> {
   //let torrent = Torrent::from_file(Path::new("DataPhilly_Creager_Thompson.key.torrent"))?;
   let torrent = Torrent::from_file(Path::new("Capture0.png.torrent"))?;
   let tracker = Tracker {
      //address: torrent.metainfo.announce.clone(),
      address: "http://10.0.0.21:6969/announce".into(),
   };
   //let files = allocate_files(&torrent)?;
   let response = tracker.announce(&torrent, EventKind::Started)?;
   println!("tracker response: {:?}", response);

   // Loop that does the following:
   //    - if there are no Connections, start trying to connect to peers in the peer list from the tracker
   //        - Remove Peer from peer list on success or failure
   //        - Keep trying peers until there are X valid connections
   //    - call update on all Connections (if any)
   //        - Update will take a Memmapped file to write to and a Bitfield indicated which pieces we have
   //        - If a piece was successfully downloaded, we need to generated a new piece number to download
   //        - So each connection will be assigned a piece to download, if the peer has that piece
   //    - if there are no updates, try to connect more peers. If no more peers in peer_list and Interval time has passed, re-announce.  Else sleep.
   // Loop is done when all pieces are downloaded

   // Assume 1 seeding peer that you can successfully connect to.

   let mut piece_index = 0;
   let mut current_peer_index = 0;

   let num_pieces = torrent.metainfo.pieces.len() as u32;
   let piece_length = torrent.metainfo.piece_length as u32;

   let listener = TcpListener::bind(format!("10.0.0.2:{}", LISTEN_PORT).as_str())?;

   // For now, just allocate file in RAM and flush to disk (this is not scalable to large torrents)
   let mut file_raw_bytes = Vec::new();
   file_raw_bytes.resize(torrent.metainfo.total_size as usize, 0 as u8);

   println!("Total file size: {}", torrent.metainfo.total_size);
   println!("Piece size: {}", torrent.metainfo.piece_length);
   println!(
      "Last Piece size: {}",
      torrent.metainfo.total_size % torrent.metainfo.piece_length
   );

   println!("Listening for connection on port {}", LISTEN_PORT);
   for stream in listener.incoming() {
      handle_client(
         stream?,
         num_pieces,
         piece_length,
         &torrent,
         &mut file_raw_bytes,
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
