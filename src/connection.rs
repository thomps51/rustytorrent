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
    total_read: usize,
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
        Self {
            am_choking: true,
            am_interested: false,
            id,
            total_read: 0,
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
            total_read: 0,
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
            num_pieces,
            state: State::Connecting,
            conn_type: Type::Outgoing,
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
        match self.state {
            State::Connected => {
                loop {
                    let retval = self.read()?;
                    match retval {
                        UpdateSuccess::NoUpdate => break,
                        UpdateSuccess::Success => continue,
                    }
                }
                // Cancel requested Requests (TODO)
                // Respond to Requests (TODO)
                if !self.peer_choking {
                    if self.block_manager.can_send_block_requests() {
                        self.block_manager.send_block_requests(
                            &mut self.stream,
                            &self.peer_has,
                            self.id,
                        )?;
                        return Ok(UpdateSuccess::Success);
                    }
                }
                Ok(UpdateSuccess::NoUpdate)
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
                debug!("Handshaking...");
                assert!(self.read_buffer.len() < Handshake::SIZE as usize);
                let length = Handshake::SIZE as usize;
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
                use crate::messages;
                let handshake_from_peer =
                    messages::Handshake::read_from(&mut (&self.read_buffer[..]))?;
                debug!("Got handshake from peer {}", self.id);
                if handshake_from_peer.peer_id == crate::PEER_ID.as_bytes() {
                    // Avoid connecting to self
                    return Err(UpdateError::CommunicationError(
                        std::io::ErrorKind::AlreadyExists.into(),
                    ));
                }
                self.peer_info.id = Some(handshake_from_peer.peer_id.to_vec());
                if let Type::Incoming = self.conn_type {
                    let handshake_to_peer =
                        messages::Handshake::new(crate::PEER_ID, &self.info_hash);
                    handshake_to_peer.write_to(&mut self.stream)?;
                }
                let msg = Unchoke {};
                msg.write_to(&mut self.stream)?;
                let msg = Interested {};
                msg.write_to(&mut self.stream)?;
                debug!("Wrote interested");
                self.state = State::Connected;
                self.read_buffer.resize(0, 0);
                Ok(UpdateSuccess::Success)
            }
        }
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
        let prev_length = self.read_buffer.len();
        let need_to_read = length - prev_length;
        if length > self.read_buffer.capacity() {
            let max_length = crate::messages::BLOCK_SIZE * 20;
            if length > max_length {
                // Some peers seem to send oversized length data (like 100 MB or more), which is
                // possibly them sending an extended handshake incorrectly (we haven't requested
                // one).  So we cap the max length at 10x block size, which allows for a bitfield
                // for files around 1 TB with 4 MB pieces.
                info!(
                    "Removing peer {:?} for oversized length request of {}",
                    &self.peer_info.id.as_ref().unwrap(),
                    length
                );
                return Err(UpdateError::CommunicationError(
                    std::io::ErrorKind::InvalidData.into(),
                ));
            }
            self.read_buffer.resize(length, 0);
        } else {
            unsafe { self.read_buffer.set_len(length) }
        }
        let read = match self.stream.read(&mut self.read_buffer[prev_length..]) {
            Ok(l) => l as usize,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    unsafe { self.read_buffer.set_len(prev_length) }
                    return Ok(UpdateSuccess::NoUpdate);
                }
                return Err(UpdateError::CommunicationError(error));
            }
        };
        if read < need_to_read {
            unsafe { self.read_buffer.set_len(prev_length + read) }
            return Ok(UpdateSuccess::NoUpdate);
        }
        self.total_read += 4 + length;
        let id = read_byte_from(&mut (&self.read_buffer[..]))? as i8;
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
        unsafe { self.read_buffer.set_len(0) }
        self.read_some = None;
        retval
    }
}

pub type UpdateResult = Result<UpdateSuccess, UpdateError>;

#[derive(Debug)]
pub enum UpdateSuccess {
    NoUpdate,
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
