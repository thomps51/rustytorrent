use bit_vec::BitVec;
use mio::net::TcpStream;
use std::io::prelude::*;

use crate::block_manager::BlockManager;
use crate::block_manager::CompletedPiece;
use crate::messages::*;
use crate::piece_store::*;
use crate::tracker::PeerInfo;
use crate::SharedPieceAssigner;
use crate::SharedPieceStore;

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
    piece_store: SharedPieceStore,
    read_some: Option<usize>,
    read_buffer: Vec<u8>,
}

fn read_byte_from<T: Read>(stream: &mut T) -> Result<u8, std::io::Error> {
    let mut buffer = [0; 1];
    stream.read_exact(&mut buffer)?;
    Ok(buffer[0])
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
        piece_assigner: SharedPieceAssigner,
        piece_store: SharedPieceStore,
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
            read_some: None,
            read_buffer: Vec::new(),
        }
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

        if !self.peer_choking {
            if self.block_manager.can_send_block_requests() {
                self.block_manager
                    .send_block_requests(&mut self.stream, &self.peer_has)?;
                return Ok(UpdateSuccess::Success);
            }
        }
        Ok(UpdateSuccess::NoUpdate)
    }

    pub fn read(&mut self) -> UpdateResult {
        let length = if let Some(v) = self.read_some {
            v
        } else {
            match self.read_u32() {
                Ok(l) => l as usize,
                Err(error) => {
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        return Ok(UpdateSuccess::NoUpdate);
                    }
                    return Err(UpdateError::CommunicationError(error));
                }
            }
        };
        self.read_some = Some(length);
        if length == 0 {
            let msg = KeepAlive {};
            self.read_some = None;
            return msg.update(self);
        }
        if self.read_buffer.len() == 0 {
            // None has been read so far
            self.read_buffer.resize(length, 0);
            let read = match self.stream.read(&mut self.read_buffer) {
                Ok(l) => l as usize,
                Err(error) => {
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        self.read_buffer.resize(0, 0);
                        return Ok(UpdateSuccess::NoUpdate);
                    }
                    return Err(UpdateError::CommunicationError(error));
                }
            };
            if read < length {
                // didn't read all
                self.read_buffer.resize(read, 0);
                return Ok(UpdateSuccess::NoUpdate);
            }
        } else {
            let prev_length = self.read_buffer.len();
            let need_to_read = length - prev_length;
            self.read_buffer.resize(length, 0);
            let read = match self.stream.read(&mut self.read_buffer[prev_length..]) {
                Ok(l) => l as usize,
                Err(error) => {
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        self.read_buffer.resize(prev_length, 0);
                        return Ok(UpdateSuccess::NoUpdate);
                    }
                    return Err(UpdateError::CommunicationError(error));
                }
            };
            if read < need_to_read {
                self.read_buffer.resize(prev_length + read, 0);
                return Ok(UpdateSuccess::NoUpdate);
            }
        }
        // if we get to here, buffer is filled
        let id = read_byte_from(&mut (&self.read_buffer[..]))? as i8;
        //println!("Received message with id: {}", id);
        macro_rules! dispatch_message ( // This is really neat!
            ($($A:ident),*) => (
                match id {
                    $($A::ID => {
                        let msg = $A::read_from(&mut (&self.read_buffer[1..]), length)?;
                        msg.update(self)
                    })*
                    _ => Err(UpdateError::UnknownMessage{id}),
                }
            );
        );
        let retval = dispatch_message!(
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
        self.read_buffer.resize(0, 0);
        self.read_some = None;
        retval
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
