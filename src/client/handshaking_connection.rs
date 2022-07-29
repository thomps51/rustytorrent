use log::{debug, info};
use mio::net::TcpStream;
use mio::{Interest, Poll, Token};

use crate::common::Sha1Hash;
use crate::common::PEER_ID;
use crate::io::ReadBuffer;
use crate::messages::*;

use super::{ConnectionBase, UpdateError};

pub struct HandshakingConnection {
    pub id: usize,
    stream: TcpStream,
    read_buffer: ReadBuffer,
    pub state: HandshakingState,
    conn_type: Type,
}

#[derive(Default)]
pub enum HandshakeUpdateSuccess {
    #[default]
    NoUpdate,
    Complete(Handshake),
}

pub enum HandshakingState {
    Connecting { info_hash: Sha1Hash },
    Reading,
    Done,
}

enum Type {
    Incoming,
    Outgoing,
}

pub type HandshakeUpdateResult = Result<HandshakeUpdateSuccess, UpdateError>;

impl HandshakingConnection {
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

    pub fn into_stream(self) -> TcpStream {
        self.stream
    }
}

impl ConnectionBase for HandshakingConnection {
    type UpdateSuccessType = HandshakeUpdateSuccess;

    fn deregister(&mut self, poll: &mut Poll) {
        poll.registry().deregister(&mut self.stream).unwrap();
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
                // if handshake_from_peer.peer_id == PEER_ID.as_bytes() {
                //     // Self connection
                //     return Err(UpdateError::CommunicationError(
                //         std::io::ErrorKind::AlreadyExists.into(),
                //     ));
                // }
                if let Type::Incoming = self.conn_type {
                    let handshake_to_peer = Handshake::new(PEER_ID, &handshake_from_peer.info_hash);
                    handshake_to_peer.write_to(&mut self.stream)?;
                }
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
