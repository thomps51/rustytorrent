use std::io::prelude::*;

use bit_vec::BitVec;
use log::{debug, info};
use mio::net::TcpStream;

use crate::block_manager::BlockManager;
use crate::hash::Sha1Hash;
use crate::messages::*;
use crate::tracker::PeerInfo;
use crate::SharedPieceAssigner;
use crate::SharedPieceStore;

pub enum State {
    Connecting,
    ReadingHandshake,
    Connected,
}

enum Type {
    Incoming,
    Outgoing,
}

pub struct Connection {
    am_choking: bool,
    am_interested: bool,
    id: usize,
    pub peer_choking: bool,
    pub peer_interested: bool,
    pub peer_has: BitVec, // Which pieces the peer has
    pub stream: TcpStream,
    last_keep_alive: std::time::Instant,
    pub block_manager: BlockManager,
    info_hash: Sha1Hash,
    pub pending_peer_requests: Vec<Request>,
    pub pending_peer_cancels: Vec<Cancel>,
    pub peer_info: PeerInfo,
    read_some: Option<usize>,
    read_buffer: Vec<u8>,
    send_buffer: Vec<u8>,
    pub num_pieces: usize,
    state: State,
    conn_type: Type,
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

    pub fn new_from_incoming(
        num_pieces: usize,
        stream: TcpStream,
        piece_assigner: SharedPieceAssigner,
        piece_store: SharedPieceStore,
        info_hash: Sha1Hash,
        id: usize,
    ) -> Connection {
        let addr = stream.peer_addr().unwrap();
        info!("New incoming connection from {}", addr);
        Self {
            am_choking: true,
            am_interested: false,
            id,
            peer_choking: true,
            peer_interested: false,
            peer_has: BitVec::from_elem(num_pieces, false),
            stream: stream,
            info_hash,
            last_keep_alive: std::time::Instant::now(),
            block_manager: BlockManager::new(piece_assigner, piece_store),
            pending_peer_requests: Vec::new(),
            pending_peer_cancels: Vec::new(),
            peer_info: PeerInfo {
                addr: addr,
                id: None,
            },
            read_some: None,
            read_buffer: Vec::new(),
            send_buffer: Vec::new(),
            num_pieces,
            state: State::ReadingHandshake,
            conn_type: Type::Incoming,
        }
    }

    pub fn new_from_outgoing(
        addr: std::net::SocketAddr,
        num_pieces: usize,
        stream: TcpStream,
        piece_assigner: SharedPieceAssigner,
        piece_store: SharedPieceStore,
        info_hash: Sha1Hash,
        id: usize,
    ) -> Connection {
        Self {
            am_choking: true,
            am_interested: false,
            id,
            peer_choking: true,
            peer_interested: false,
            peer_has: BitVec::from_elem(num_pieces, false),
            stream: stream,
            last_keep_alive: std::time::Instant::now(),
            block_manager: BlockManager::new(piece_assigner, piece_store),
            pending_peer_requests: Vec::new(),
            pending_peer_cancels: Vec::new(),
            peer_info: PeerInfo {
                addr: addr,
                id: None,
            },
            info_hash,
            read_some: None,
            read_buffer: Vec::new(),
            send_buffer: Vec::new(),
            num_pieces,
            state: State::Connecting,
            conn_type: Type::Outgoing,
        }
    }

    fn read(&mut self) -> UpdateResult {
        let length = if let Some(v) = self.read_some {
            v
        } else {
            const LENGTH_BYTE_SIZE: usize = 4;
            let retval = self.read_exact_into_buffer(LENGTH_BYTE_SIZE);
            match retval {
                Ok(UpdateSuccess::Success) => {}
                _ => return retval,
            }
            let value: u32 = crate::messages::read_as_be(&mut &self.read_buffer[..]).unwrap();
            self.read_buffer.clear();
            value as usize
        };
        self.read_some = Some(length);
        if length == 0 {
            let msg = KeepAlive {};
            self.read_some = None;
            return msg.update(self);
        }
        let retval = self.read_exact_into_buffer(length);
        match retval {
            Ok(UpdateSuccess::Success) => {}
            _ => return retval,
        }
        let total_read = 4 + length;
        let id = read_byte_from(&mut (&self.read_buffer[..]))? as i8;
        macro_rules! dispatch_message ( // This is really neat!
            ($($A:ident),*) => (
                match id {
                    Block::ID => {
                        Block::read_and_update(&mut (&self.read_buffer[1..]), &mut self.block_manager, length)?;
                        Ok(UpdateSuccess::Success)
                    },
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
            Cancel
        );
        unsafe { self.read_buffer.set_len(0) }
        self.read_some = None;
        match retval {
            Ok(UpdateSuccess::Success) => Ok(UpdateSuccess::Transferred {
                downloaded: total_read,
                uploaded: 0,
            }),
            _ => retval,
        }
    }

    // Read until EWOULDBLOCK or error occurs
    fn read_all(&mut self) -> UpdateResult {
        let mut total_downloaded = 0;
        let mut total_uploaded = 0;
        loop {
            let retval = self.read()?;
            match retval {
                UpdateSuccess::NoUpdate => {
                    break;
                }
                UpdateSuccess::Transferred {
                    downloaded,
                    uploaded,
                } => {
                    total_downloaded += downloaded;
                    total_uploaded += uploaded;
                    continue;
                }
                UpdateSuccess::Success => continue,
            }
        }
        if total_downloaded != 0 {
            Ok(UpdateSuccess::Transferred {
                downloaded: total_downloaded,
                uploaded: total_uploaded,
            })
        } else {
            Ok(UpdateSuccess::NoUpdate)
        }
    }

    fn read_exact_into_buffer(&mut self, length: usize) -> UpdateResult {
        let prev_length = self.read_buffer.len();
        let need_to_read = length - prev_length;
        if length > self.read_buffer.capacity() {
            const MAX_LENGTH: usize = crate::messages::BLOCK_SIZE * 20;
            if length > MAX_LENGTH {
                // Protect against nonsense data since we will size our buffer based on this
                return Err(UpdateError::CommunicationError(
                    std::io::ErrorKind::InvalidData.into(),
                ));
            }
            self.read_buffer.resize(length, 0);
        } else {
            // Safe because of the above check
            unsafe { self.read_buffer.set_len(length) }
        }
        let read = match self.stream.read(&mut self.read_buffer[prev_length..]) {
            Ok(l) => l as usize,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    // Safe because prev_length < length
                    unsafe { self.read_buffer.set_len(prev_length) }
                    return Ok(UpdateSuccess::NoUpdate);
                }
                return Err(UpdateError::CommunicationError(error));
            }
        };
        if read < need_to_read {
            // Safe because prev_length + read < length
            unsafe { self.read_buffer.set_len(prev_length + read) }
            return Ok(UpdateSuccess::NoUpdate);
        }
        Ok(UpdateSuccess::Success)
    }

    // True: message was used
    // False: message was not used
    //
    pub fn send<T: Message>(&mut self, message: &T) -> Result<bool, std::io::Error> {
        let mut result = false;
        if self.send_buffer.len() == 0 {
            message.write_to(&mut self.send_buffer).unwrap();
            result = true;
        }
        match self.stream.write(&self.send_buffer) {
            Ok(sent) => {
                if sent == self.send_buffer.len() {
                    self.send_buffer.clear();
                } else {
                    self.send_buffer.drain(0..sent);
                }
            }
            Err(error) => {
                if error.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(error);
                }
            }
        }
        Ok(result)
    }

    fn send_block_requests(&mut self) -> UpdateResult {
        if !self.peer_choking {
            self.block_manager
                .send_block_requests(&mut self.stream, &self.peer_has, self.id)?;
            return Ok(UpdateSuccess::Success);
        }
        Ok(UpdateSuccess::NoUpdate)
    }

    // Need to indicate the following:
    //     - Error while updating, peer either is disconnected or needs to be disconnected
    //     - There was no update to do (no new messages)
    //     - Successfully did things, but no complete piece
    //     - Successfully downloaded a full piece
    pub fn update(&mut self) -> UpdateResult {
        match self.state {
            State::Connected => {
                let retval = self.read_all();
                // Cancel requested Requests (TODO)
                // Respond to Requests (TODO)
                self.send_block_requests()?;
                retval
            }
            State::Connecting => {
                debug!("Connections for connection {}", self.id);
                assert_eq!(self.read_buffer.len(), 0);
                // Assumes update has been called because Poll indicated that this socket is now
                // connected, or that connection has failed
                use crate::messages;
                let handshake_to_peer = messages::Handshake::new(crate::PEER_ID, &self.info_hash);
                handshake_to_peer.write_to(&mut self.stream)?;
                self.state = State::ReadingHandshake;
                Ok(UpdateSuccess::Success)
            }
            State::ReadingHandshake => {
                let retval = self.read_exact_into_buffer(Handshake::SIZE as usize);
                match retval {
                    Ok(UpdateSuccess::Success) => {}
                    _ => return retval,
                }
                let handshake_from_peer = Handshake::read_from(&mut (&self.read_buffer[..]))?;
                debug!("Got handshake from peer {}", self.id);
                if handshake_from_peer.peer_id == crate::PEER_ID.as_bytes() {
                    // Avoid connecting to self
                    return Err(UpdateError::CommunicationError(
                        std::io::ErrorKind::AlreadyExists.into(),
                    ));
                }
                self.peer_info.id = Some(handshake_from_peer.peer_id.to_vec());
                if let Type::Incoming = self.conn_type {
                    let handshake_to_peer = Handshake::new(crate::PEER_ID, &self.info_hash);
                    handshake_to_peer.write_to(&mut self.stream)?;
                }
                let msg = Unchoke {};
                msg.write_to(&mut self.stream)?;
                let msg = Interested {};
                msg.write_to(&mut self.stream)?;
                self.state = State::Connected;
                self.read_buffer.resize(0, 0);
                self.read_buffer.clear();
                Ok(UpdateSuccess::Success)
            }
        }
    }
}

pub type UpdateResult = Result<UpdateSuccess, UpdateError>;

#[derive(Debug)]
pub enum UpdateSuccess {
    NoUpdate,
    Transferred { downloaded: usize, uploaded: usize },
    Success,
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
