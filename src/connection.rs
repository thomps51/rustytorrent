use bit_vec::BitVec;
use std::cell::RefCell;
use std::io::prelude::*;
use std::net::TcpStream;
use std::rc::Rc;

use crate::message::*;
use crate::tracker::PeerInfo;

pub struct Connection {
    am_choking: bool,
    am_interested: bool,
    pub peer_choking: bool,
    pub peer_interested: bool,
    pub peer_has: BitVec, // Which pieces the peer has
    pub stream: TcpStream,
    last_keep_alive: std::time::Instant,
    pub piece_in_flight: PieceInFlight,
    pub pending_peer_requests: Vec<Request>,
    pub pending_peer_cancels: Vec<Cancel>,
    pub peer_info: PeerInfo,
    piece_assigner: Rc<RefCell<crate::PieceAssigner>>,
}

impl Connection {
    pub fn received_keep_alive(&mut self) {
        self.last_keep_alive = std::time::Instant::now();
    }

    pub fn new(
        num_pieces: u32,
        peer_info: PeerInfo,
        piece_assigner: Rc<RefCell<crate::PieceAssigner>>,
    ) -> Result<Connection, std::io::Error> {
        let (first_piece, piece_length) = piece_assigner.borrow_mut().get();
        Ok(Self {
            am_choking: true,
            am_interested: false,
            peer_choking: true,
            peer_interested: false,
            peer_has: BitVec::from_elem(num_pieces as usize, false),
            stream: TcpStream::connect(peer_info.addr)?,
            last_keep_alive: std::time::Instant::now(),
            piece_in_flight: PieceInFlight::new(piece_length, first_piece),
            pending_peer_requests: Vec::new(),
            pending_peer_cancels: Vec::new(),
            peer_info: peer_info.clone(),
            piece_assigner: piece_assigner,
        })
    }

    pub fn new_from_stream(
        num_pieces: u32,
        peer_id: &[u8],
        stream: TcpStream,
        piece_assigner: Rc<RefCell<crate::PieceAssigner>>,
    ) -> Connection {
        let (first_piece, piece_length) = piece_assigner.borrow_mut().get();
        let addr = stream.peer_addr().unwrap();
        Self {
            am_choking: true,
            am_interested: false,
            peer_choking: true,
            peer_interested: false,
            peer_has: BitVec::from_elem(num_pieces as usize, false),
            stream: stream,
            last_keep_alive: std::time::Instant::now(),
            piece_in_flight: PieceInFlight::new(piece_length, first_piece),
            pending_peer_requests: Vec::new(),
            pending_peer_cancels: Vec::new(),
            peer_info: PeerInfo {
                addr: addr,
                id: peer_id.to_owned(),
            },
            piece_assigner: piece_assigner,
        }
    }

    fn read_byte(&mut self) -> Result<u8, std::io::Error> {
        let mut buffer = [0; 1];
        self.stream.read_exact(&mut buffer)?;
        Ok(buffer[0])
    }

    fn read_u32(&mut self) -> Result<u32, std::io::Error> {
        let mut buffer = [0; 4];
        self.stream.read_exact(&mut buffer)?;
        Ok(u32::from_be_bytes(buffer))
    }

    // Need to indicate the following:
    //     - Error while updating, peer either is disconnected or needs to be disconnected
    //     - There was no update to do (no new messages)
    //     - Successfully did things, but no complete piece
    //     - Successfully downloaded a full piece
    pub fn update(&mut self, have: &mut BitVec) -> UpdateResult {
        // Read in a loop until one of the following conditions are met:
        //     - An error occurs
        //     - There are no new messages to read (NoUpdate)
        //     - A full piece has been assembled
        loop {
            let retval = self.read()?;
            match retval {
                UpdateSuccess::NoUpdate => break,
                UpdateSuccess::PieceComplete(piece) => {
                    let mut piece_assigner = self.piece_assigner.borrow_mut();
                    if piece_assigner.has_pieces() {
                        let (index, size) = piece_assigner.get();
                        self.piece_in_flight = PieceInFlight::new(size, index);
                    }
                    have.set(piece.index as usize, true);
                    return Ok(UpdateSuccess::PieceComplete(piece));
                }
                UpdateSuccess::Success => continue,
            }
        }
        // Cancel requested Requests (TODO)
        // Respond to Requests (TODO)

        // Make a request if interested/choke conditions are met
        if !self.peer_choking {
            if let Some(value) = self.piece_in_flight.get_block_request() {
                println!(
                    "requesting block {} of {}",
                    self.piece_in_flight.current_block, self.piece_in_flight.num_blocks
                );
                value.write_to(&mut self.stream)?;
            }
        }

        Ok(UpdateSuccess::NoUpdate)
        //Ok(UpdateSuccess::Success)
    }

    pub fn read(&mut self) -> UpdateResult {
        self.stream.set_nonblocking(true).unwrap();
        let length = match self.read_u32() {
            Ok(l) => l,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    //println!("No messages to read from peer");
                    return Ok(UpdateSuccess::NoUpdate);
                }
                return Err(UpdateError::CommunicationError(error));
            }
        };
        if length == 0 {
            let msg = KeepAlive {};
            return msg.update(self);
        }
        self.stream.set_nonblocking(false).unwrap();
        let id = self.read_byte()? as i8;
        println!("Received message with id: {}", id);
        macro_rules! dispatch_message (
            ($($A:ident),*) => (
                match id {
                    $($A::ID => {
                        let msg = $A::read_from(&mut self.stream, length)?;
                        msg.update(self)
                    })*
                    _ => Err(UpdateError::UnknownMessage{id}),
                }
            );
        );
        return dispatch_message!(
            Choke,
            Unchoke,
            Interested,
            NotInterested,
            Have,
            Bitfield,
            Request,
            Block,
            Cancel
        );
    }
}

pub type UpdateResult = Result<UpdateSuccess, UpdateError>;

#[derive(Debug)]
pub enum UpdateSuccess {
    NoUpdate,
    Success,
    PieceComplete(CompletedPiece),
}

#[derive(Debug)]
pub enum UpdateError {
    CommunicationError(std::io::Error),
    UnknownMessage { id: i8 },
    IndexOutOfBounds,
}

impl std::error::Error for UpdateError {}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&format!("{:?}", self))
    }
}

impl From<std::io::Error> for UpdateError {
    fn from(error: std::io::Error) -> Self {
        UpdateError::CommunicationError(error)
    }
}

#[derive(Debug)]
pub struct CompletedPiece {
    pub index: u32,
    pub piece: Vec<u8>,
}

pub struct PieceInFlight {
    index: u32,
    piece: Vec<u8>,
    have: BitVec,
    last_block_length: u32,
    num_blocks: u32,
    current_block: u32,
    block_requested: bool,
}

impl PieceInFlight {
    const BLOCK_SIZE: u32 = 1 << 14; // 16 KiB

    pub fn add_block(&mut self, block: &Block) -> Option<CompletedPiece> {
        self.current_block += 1;
        self.block_requested = false;
        if block.index != self.index {
            panic!("Error! Block is for a different piece!");
        }
        if block.begin % Self::BLOCK_SIZE != 0 {
            panic!("Error! Begin is in between blocks!");
        }
        let block_index = block.begin / Self::BLOCK_SIZE;
        if block_index >= self.num_blocks {
            panic!("Error! Out of range");
        }
        let block_length = if block_index == self.num_blocks - 1 {
            self.last_block_length
        } else {
            Self::BLOCK_SIZE
        };
        println!("Received block {} of {}", block_index + 1, self.num_blocks);
        for i in 0..block_length {
            let offset = block.begin + i;
            self.piece[offset as usize] = block.block[i as usize];
        }
        self.have.set(block_index as usize, true);
        if self.have.all() {
            Some(CompletedPiece {
                index: self.index,
                piece: self.piece.clone(),
            })
        } else {
            None
        }
    }

    pub fn get_block_request(&mut self) -> Option<Request> {
        if self.block_requested {
            return None;
        }
        self.block_requested = true;
        if self.current_block >= self.num_blocks {
            return None;
        }
        let begin = self.current_block * Self::BLOCK_SIZE;
        let length = if self.current_block == self.num_blocks - 1 {
            self.last_block_length
        } else {
            Self::BLOCK_SIZE
        };
        Some(Request {
            index: self.index,
            begin,
            length,
        })
    }

    fn new(piece_size: u32, index: u32) -> PieceInFlight {
        println!(
            "PieceInFlight::new() with piece_size {} and index {}",
            piece_size, index
        );
        let (last_block_length, num_blocks) = if piece_size % Self::BLOCK_SIZE == 0 {
            (Self::BLOCK_SIZE, piece_size / Self::BLOCK_SIZE)
        } else {
            (
                piece_size % Self::BLOCK_SIZE,
                (piece_size / Self::BLOCK_SIZE) + 1,
            )
        };

        let mut piece = Vec::new();
        piece.resize(piece_size as usize, 0);
        let have = BitVec::from_elem(num_blocks as usize, false);
        println!(
            "Current piece in flight has {} blocks of size {} for piece of size {}",
            num_blocks,
            Self::BLOCK_SIZE,
            piece_size
        );
        PieceInFlight {
            index,
            piece,
            have,
            last_block_length,
            num_blocks,
            current_block: 0,
            block_requested: false,
        }
    }
}
