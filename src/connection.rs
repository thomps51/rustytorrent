use std::io::prelude::*;

use bit_vec::BitVec;
use log::{debug, info};
use mio::net::TcpStream;
use mio::{Interest, Poll, Token};

use crate::block_manager::BlockManager;
use crate::constants::PEER_ID;
use crate::hash::Sha1Hash;
use crate::messages::*;
use crate::read_buffer::ReadBuffer;
use crate::tracker::PeerInfo;
use crate::SharedPieceAssigner;
use crate::SharedPieceStore;

pub enum State {
    Connecting,
    ReadingHandshake,
    ConnectedNormal,
    ConnectedEndgame,
}

enum Type {
    Incoming,
    Outgoing,
}

pub struct Connection {
    am_choking: bool,
    am_interested: bool,
    pub id: usize,
    pub peer_choking: bool,
    pub peer_interested: bool,
    pub peer_has: BitVec,
    pub stream: TcpStream,
    last_keep_alive: std::time::Instant,
    pub block_manager: BlockManager,
    info_hash: Sha1Hash,
    pub pending_peer_requests: Vec<Request>,
    pub pending_peer_cancels: Vec<Cancel>,
    peer_info: PeerInfo,
    pub next_message_length: Option<usize>,
    pub read_buffer: ReadBuffer,
    send_buffer: Vec<u8>,
    pub num_pieces: usize,
    pub state: State,
    conn_type: Type,
    pub downloaded: usize,
}

fn read_byte_from<T: Read>(stream: &mut T) -> Result<u8, std::io::Error> {
    let mut buffer = [0; 1];
    stream.read_exact(&mut buffer)?;
    Ok(buffer[0])
}

impl Connection {
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
            next_message_length: None,
            read_buffer: ReadBuffer::new(1 << 20), // 1 MiB
            send_buffer: Vec::new(),
            num_pieces,
            state: State::ReadingHandshake,
            conn_type: Type::Incoming,
            downloaded: 0,
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
            next_message_length: None,
            read_buffer: ReadBuffer::new(1 << 20), // 1 MiB
            send_buffer: Vec::new(),
            num_pieces,
            state: State::Connecting,
            conn_type: Type::Outgoing,
            downloaded: 0,
        }
    }

    pub fn deregister(&mut self, poll: &mut Poll) {
        poll.registry().deregister(&mut self.stream).unwrap();
    }

    fn read(&mut self) -> UpdateResult {
        const LENGTH_BYTE_SIZE: usize = 4;
        let length = if let Some(v) = self.next_message_length {
            v
        } else {
            if !self
                .read_buffer
                .read_at_least_from(LENGTH_BYTE_SIZE, &mut self.stream)?
            {
                return Ok(UpdateSuccess::NoUpdate);
            }
            let value = read_as_be::<u32, _, _>(&mut self.read_buffer).unwrap();
            value
        };
        self.next_message_length = Some(length);
        if length == 0 {
            let msg = KeepAlive {};
            self.next_message_length = None;
            return msg.update(self);
        }
        if !self
            .read_buffer
            .read_at_least_from(length, &mut self.stream)?
        {
            return Ok(UpdateSuccess::NoUpdate);
        }
        let total_read = LENGTH_BYTE_SIZE + length;
        let id = read_byte_from(&mut self.read_buffer)? as i8;
        macro_rules! dispatch_message ( // This is really neat!
            ($($A:ident),*) => (
                match id {
                    Block::ID => {
                        Block::read_and_update(&mut self.read_buffer, &mut self.block_manager, length)?;
                        Ok(UpdateSuccess::Success)
                    },
                    $($A::ID => {
                        let msg = $A::read_from(&mut self.read_buffer, length)?;
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
            Cancel,
            Port
        );
        self.next_message_length = None;
        self.downloaded += total_read;
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

    pub fn received_keep_alive(&mut self) {
        self.last_keep_alive = std::time::Instant::now();
    }

    pub fn register(&mut self, poll: &mut Poll, token: Token, interests: Interest) {
        poll.registry()
            .register(&mut self.stream, token, interests)
            .unwrap();
    }

    pub fn reregister(&mut self, poll: &mut Poll, token: Token, interests: Interest) {
        poll.registry()
            .reregister(&mut self.stream, token, interests)
            .unwrap();
    }

    // True: message was used
    // False: message was dropped because of TCP pushback
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
        //self.stream.flush()?;
        Ok(result)
    }

    fn send_block_requests(&mut self) -> UpdateResult {
        if !self.peer_choking {
            let sent = self.block_manager.send_block_requests(
                &mut self.stream,
                &self.peer_has,
                self.id,
            )?;
            if sent == 0 {
                return Ok(UpdateSuccess::NoUpdate);
            }
            return Ok(UpdateSuccess::Success);
        }
        Ok(UpdateSuccess::NoUpdate)
    }

    pub fn update(&mut self) -> UpdateResult {
        match self.state {
            State::ConnectedEndgame => {
                let read_result = self.read_all()?;
                // Cancel requested Requests (TODO)
                // Respond to Requests (TODO)
                return Ok(read_result);
            }
            State::ConnectedNormal => {
                let read_result = self.read_all()?;
                // Cancel requested Requests (TODO)
                // Respond to Requests (TODO)
                if self.block_manager.piece_assigner.borrow().is_endgame() {
                    self.state = State::ConnectedEndgame;
                    return Ok(read_result);
                }
                debug!("Connection {} sending block requests", self.id);
                let request_result = self.send_block_requests()?;
                match (&read_result, request_result) {
                    (UpdateSuccess::NoUpdate, UpdateSuccess::NoUpdate) => {
                        Ok(UpdateSuccess::NoUpdate)
                    }
                    (UpdateSuccess::NoUpdate, UpdateSuccess::Success) => Ok(UpdateSuccess::Success),
                    (_, _) => Ok(read_result),
                }
            }
            State::Connecting => {
                debug!("Connections for connection {}", self.id);
                // Assumes update has been called because Poll indicated that this socket is now
                // connected, or that connection has failed
                use crate::messages;
                let handshake_to_peer = messages::Handshake::new(PEER_ID, &self.info_hash);
                handshake_to_peer.write_to(&mut self.stream)?;
                self.state = State::ReadingHandshake;
                Ok(UpdateSuccess::Success)
            }
            State::ReadingHandshake => {
                let length = Handshake::SIZE as usize;
                if !self
                    .read_buffer
                    .read_at_least_from(length, &mut self.stream)?
                {
                    return Ok(UpdateSuccess::NoUpdate);
                }
                let handshake_from_peer = Handshake::read_from(&mut self.read_buffer)?;
                debug!("Got handshake from peer {}", self.id);
                if handshake_from_peer.peer_id == PEER_ID.as_bytes() {
                    return Err(UpdateError::CommunicationError(
                        std::io::ErrorKind::AlreadyExists.into(),
                    ));
                }
                self.peer_info.id = Some(handshake_from_peer.peer_id.to_vec());
                if let Type::Incoming = self.conn_type {
                    let handshake_to_peer = Handshake::new(PEER_ID, &self.info_hash);
                    handshake_to_peer.write_to(&mut self.stream)?;
                }
                let msg = Unchoke {};
                msg.write_to(&mut self.stream)?;
                let msg = Interested {};
                msg.write_to(&mut self.stream)?;
                self.state = State::ConnectedNormal;
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
