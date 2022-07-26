use std::io::prelude::*;

use bit_vec::BitVec;
use log::{debug, info};
use mio::net::TcpStream;
use mio::{Interest, Poll, Token};

use super::block_manager::BlockManager;
use crate::common::Sha1Hash;
use crate::common::SharedPieceAssigner;
use crate::common::SharedPieceStore;
use crate::common::PEER_ID;
use crate::io::ReadBuffer;
use crate::messages::*;

pub enum HandshakingState {
    Connecting { info_hash: Sha1Hash },
    Reading,
    Done,
}

pub enum State {
    ConnectedNormal,
    ConnectedEndgame,
    Seeding,
}

enum Type {
    Incoming,
    Outgoing,
}

pub enum Connection {
    Handshaking(HandshakingConnection),
    Established(EstablishedConnection),
}

#[derive(Default)]
pub enum HandshakeUpdateSuccess {
    #[default]
    NoUpdate,
    Complete(Handshake),
}

pub type HandshakeUpdateResult = Result<HandshakeUpdateSuccess, UpdateError>;

pub trait ConnectionBase: Sized {
    type UpdateSuccessType: Default;

    fn deregister(&mut self, poll: &mut Poll);

    fn register(&mut self, poll: &mut Poll, token: Token, interests: Interest);

    fn reregister(&mut self, poll: &mut Poll, token: Token, interests: Interest);

    fn update(&mut self) -> Result<Self::UpdateSuccessType, UpdateError>;
}

pub struct HandshakingConnection {
    pub id: usize,
    stream: TcpStream,
    read_buffer: ReadBuffer,
    pub state: HandshakingState,
    conn_type: Type,
}

impl ConnectionBase for HandshakingConnection {
    type UpdateSuccessType = HandshakeUpdateSuccess;

    fn deregister(&mut self, poll: &mut Poll) {
        poll.registry().deregister(&mut self.stream).unwrap();
    }

    fn register(&mut self, poll: &mut Poll, token: Token, interests: Interest) {
        poll.registry()
            .register(&mut self.stream, token, interests)
            .unwrap();
    }

    fn reregister(&mut self, poll: &mut Poll, token: Token, interests: Interest) {
        poll.registry()
            .reregister(&mut self.stream, token, interests)
            .unwrap();
    }

    fn update(&mut self) -> Result<Self::UpdateSuccessType, UpdateError> {
        match self.state {
            HandshakingState::Connecting { info_hash } => {
                debug!("Connections for connection {}", self.id);
                // Assumes update has been called because Poll indicated that this socket is now
                // connected, or that connection has failed
                use crate::messages;
                let handshake_to_peer = messages::Handshake::new(PEER_ID, &info_hash);
                handshake_to_peer.write_to(&mut self.stream)?;
                self.state = HandshakingState::Reading;
                Ok(HandshakeUpdateSuccess::NoUpdate)
            }
            HandshakingState::Reading => {
                let length = Handshake::SIZE as usize;
                if !self
                    .read_buffer
                    .read_at_least_from(length, &mut self.stream)?
                {
                    return Ok(HandshakeUpdateSuccess::NoUpdate);
                }
                let handshake_from_peer = Handshake::read_from(&mut self.read_buffer)?;
                debug!("Got handshake from peer {}", self.id);
                if handshake_from_peer.peer_id == PEER_ID.as_bytes() {
                    // Self connection
                    return Err(UpdateError::CommunicationError(
                        std::io::ErrorKind::AlreadyExists.into(),
                    ));
                }
                if let Type::Incoming = self.conn_type {
                    // TODO: reject if incoming hash does not match known torrent hash
                    let handshake_to_peer = Handshake::new(PEER_ID, &handshake_from_peer.info_hash);
                    handshake_to_peer.write_to(&mut self.stream)?;
                }
                let msg = Unchoke {};
                msg.write_to(&mut self.stream)?;
                let msg = Interested {};
                msg.write_to(&mut self.stream)?;
                self.state = HandshakingState::Done;
                Ok(HandshakeUpdateSuccess::Complete(handshake_from_peer))
            }
            HandshakingState::Done => panic!("Update should not be called in this state"),
        }
    }
}

impl HandshakingConnection {
    pub fn new_from_incoming(stream: TcpStream, id: usize) -> Self {
        let addr = stream.peer_addr().unwrap();
        info!("New incoming connection from {}", addr);
        Self {
            id,
            stream,
            read_buffer: ReadBuffer::new(1 << 20), // 1 MiB
            state: HandshakingState::Reading,
            conn_type: Type::Incoming,
        }
    }

    pub fn new_from_outgoing(stream: TcpStream, info_hash: Sha1Hash, id: usize) -> Self {
        Self {
            id,
            stream: stream,
            read_buffer: ReadBuffer::new(1 << 20), // 1 MiB
            state: HandshakingState::Connecting { info_hash },
            conn_type: Type::Outgoing,
        }
    }
}

//pub struct EstablishedConnection {
pub struct EstablishedConnection {
    am_choking: bool,
    am_interested: bool,
    pub id: usize,
    pub peer_choking: bool,
    pub peer_interested: bool,
    pub peer_has: BitVec,
    stream: TcpStream,
    last_keep_alive: std::time::Instant,
    block_manager: BlockManager,
    pub pending_peer_requests: Vec<Request>,
    pub pending_peer_cancels: Vec<Cancel>,
    next_message_length: Option<usize>,
    read_buffer: ReadBuffer,
    send_buffer: Vec<u8>,
    pub num_pieces: usize,
    pub state: State,
    pub downloaded: usize,
}

fn read_byte_from<T: Read>(stream: &mut T) -> Result<u8, std::io::Error> {
    let mut buffer = [0; 1];
    stream.read_exact(&mut buffer)?;
    Ok(buffer[0])
}

impl ConnectionBase for EstablishedConnection {
    type UpdateSuccessType = UpdateSuccess;

    fn deregister(&mut self, poll: &mut Poll) {
        poll.registry().deregister(&mut self.stream).unwrap();
    }

    fn register(&mut self, poll: &mut Poll, token: Token, interests: Interest) {
        poll.registry()
            .register(&mut self.stream, token, interests)
            .unwrap();
    }

    fn reregister(&mut self, poll: &mut Poll, token: Token, interests: Interest) {
        poll.registry()
            .reregister(&mut self.stream, token, interests)
            .unwrap();
    }

    fn update(&mut self) -> UpdateResult {
        match self.state {
            State::Seeding => {
                let read_result = self.read_all()?;
                let to_send: Vec<Request> = self.pending_peer_requests.drain(..).collect();
                for request in to_send {
                    // don't bother checking result, if they give us TCP pushback, it's their own damn fault
                    if !self.send(&request)? {
                        break;
                    }
                }
                Ok(read_result)
            }
            State::ConnectedNormal | State::ConnectedEndgame => {
                let read_result = self.read_all()?;
                // Cancel requested Requests (TODO)
                if !self.pending_peer_requests.is_empty() {
                    let to_send: Vec<Request> = self.pending_peer_requests.drain(..).collect();
                    for request in to_send {
                        // don't bother checking result, if they give us TCP pushback, it's their own damn fault
                        if !self.send(&request)? {
                            break;
                        }
                    }
                }
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
        }
    }
}

impl EstablishedConnection {
    pub fn new_from_handshaking(
        connection: HandshakingConnection,
        num_pieces: usize,
        piece_assigner: SharedPieceAssigner,
        piece_store: SharedPieceStore,
    ) -> Self {
        Self {
            am_choking: true,
            am_interested: false,
            id: connection.id,
            peer_choking: true,
            peer_interested: false,
            peer_has: BitVec::from_elem(num_pieces, false),
            stream: connection.stream,
            last_keep_alive: std::time::Instant::now(),
            block_manager: BlockManager::new(piece_assigner, piece_store),
            pending_peer_requests: Vec::new(),
            pending_peer_cancels: Vec::new(),
            next_message_length: None,
            read_buffer: connection.read_buffer,
            send_buffer: Vec::new(),
            num_pieces,
            state: State::ConnectedNormal,
            downloaded: 0,
        }
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
}

pub type UpdateResult = Result<UpdateSuccess, UpdateError>;

#[derive(Debug, Default)]
pub enum UpdateSuccess {
    #[default]
    NoUpdate,
    Transferred {
        downloaded: usize,
        uploaded: usize,
    },
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
