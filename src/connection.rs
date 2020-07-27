use bit_vec::BitVec;
use std::cell::RefCell;
use std::io::prelude::*;
use std::net::TcpStream;
use std::rc::Rc;

use crate::block_manager::BlockManager;
use crate::block_manager::CompletedPiece;
use crate::message::*;
use crate::piece_assigner::PieceAssigner;
use crate::piece_store::*;
use crate::tracker::PeerInfo;

pub struct Connection {
    am_choking: bool,
    am_interested: bool,
    pub peer_choking: bool,
    pub peer_interested: bool,
    pub peer_has: BitVec, // Which pieces the peer has
    pub stream: TcpStream,
    last_keep_alive: std::time::Instant,
    pub block_manager: BlockManager,
    pub pending_peer_requests: Vec<Request>,
    pub pending_peer_cancels: Vec<Cancel>,
    pub peer_info: PeerInfo,
    piece_store: Rc<RefCell<FileSystemPieceStore>>,
}

impl Connection {
    pub fn received_keep_alive(&mut self) {
        self.last_keep_alive = std::time::Instant::now();
    }

    /*
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
            block_manager: BlockManager::new(piece_assigner),
            pending_peer_requests: Vec::new(),
            pending_peer_cancels: Vec::new(),
            peer_info: peer_info.clone(),
        })
    }
    */

    // constructor that takes a stream, used when you get connected to via a listen
    pub fn new_from_stream(
        num_pieces: usize,
        peer_id: &[u8],
        stream: TcpStream,
        piece_assigner: Rc<RefCell<PieceAssigner>>,
        piece_store: Rc<RefCell<FileSystemPieceStore>>,
    ) -> Connection {
        let addr = stream.peer_addr().unwrap();
        Self {
            am_choking: true,
            am_interested: false,
            peer_choking: true,
            peer_interested: false,
            peer_has: BitVec::from_elem(num_pieces as usize, false),
            stream: stream,
            last_keep_alive: std::time::Instant::now(),
            block_manager: BlockManager::new(piece_assigner),
            pending_peer_requests: Vec::new(),
            pending_peer_cancels: Vec::new(),
            peer_info: PeerInfo {
                addr: addr,
                id: peer_id.to_owned(),
            },
            piece_store,
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
    pub fn update(&mut self) -> UpdateResult {
        // Read in a loop until one of the following conditions are met:
        //     - An error occurs
        //     - There are no new messages to read (NoUpdate)
        //     - A full piece has been assembled
        loop {
            let retval = self.read()?;
            match retval {
                UpdateSuccess::NoUpdate => break,
                UpdateSuccess::PieceComplete(piece) => {
                    self.piece_store.borrow_mut().write(piece)?;
                    return Ok(UpdateSuccess::Success);
                }
                UpdateSuccess::Success => continue,
            }
        }
        // Cancel requested Requests (TODO)
        // Respond to Requests (TODO)

        // Make request(s) if interested/choke conditions are met
        if !self.peer_choking {
            if self.block_manager.can_send_block_requests() {
                self.block_manager.send_block_requests(&mut self.stream)?;
                return Ok(UpdateSuccess::Success);
            }
        }
        Ok(UpdateSuccess::NoUpdate)
    }

    pub fn read(&mut self) -> UpdateResult {
        self.stream.set_nonblocking(true).unwrap();
        let length = match self.read_u32() {
            Ok(l) => l,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::WouldBlock {
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
