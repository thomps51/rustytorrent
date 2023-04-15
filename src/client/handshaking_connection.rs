use log::debug;
use mio::{Interest, Poll, Token};
use write_to::WriteTo;

use crate::common::{Sha1Hash, PEER_ID_LENGTH};
use crate::io::ReadBuffer;
use crate::messages::*;

use super::{ConnectionBase, NetworkSource, UpdateError};

pub struct HandshakingConnection {
    pub token: Token,
    stream: NetworkSource,
    read_buffer: ReadBuffer,
    pub state: HandshakingState,
    conn_type: Type,
    peer_id: [u8; PEER_ID_LENGTH],
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

impl ConnectionBase for HandshakingConnection {
    type UpdateSuccessType = HandshakeUpdateSuccess;

    fn into_network_source(self) -> NetworkSource {
        self.stream
    }

    fn update(
        &mut self,
        _read_buffer: &mut ReadBuffer, // Use our own buffer with the exact handshake size
    ) -> Result<Self::UpdateSuccessType, UpdateError> {
        match self.state {
            HandshakingState::Connecting { info_hash } => {
                debug!("Connections for connection {}", self.token.0);
                // Assumes update has been called because Poll indicated that this socket is now
                // connected, or that connection has failed
                use crate::messages;
                let handshake_to_peer = messages::Handshake::new(self.peer_id, &info_hash);
                handshake_to_peer.write_to(&mut self.stream)?;
                self.state = HandshakingState::Reading;
                debug!("Finished writing handshake");
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
                debug!("Got handshake from peer {}", self.token.0);
                if let Type::Incoming = self.conn_type {
                    let handshake_to_peer =
                        Handshake::new(self.peer_id, &handshake_from_peer.info_hash);
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
    pub fn new_from_incoming(
        stream: NetworkSource,
        token: Token,
        peer_id: [u8; PEER_ID_LENGTH],
    ) -> Self {
        debug!("New incoming connection from {:?}", stream.peer_addr());
        Self {
            token,
            stream,
            read_buffer: ReadBuffer::new(Handshake::SIZE as usize),
            state: HandshakingState::Reading,
            conn_type: Type::Incoming,
            peer_id,
        }
    }

    pub fn new_from_outgoing(
        stream: NetworkSource,
        info_hash: Sha1Hash,
        token: Token,
        peer_id: [u8; PEER_ID_LENGTH],
    ) -> Self {
        Self {
            token,
            stream,
            read_buffer: ReadBuffer::new(Handshake::SIZE as usize),
            state: HandshakingState::Connecting { info_hash },
            conn_type: Type::Outgoing,
            peer_id,
        }
    }

    pub fn register(&mut self, poll: &mut Poll, token: Token, interests: Interest) {
        poll.registry()
            .register(&mut self.stream, token, interests)
            .unwrap();
    }

    pub fn reregister(&mut self, poll: &Poll, token: Token, interests: Interest) {
        poll.registry()
            .reregister(&mut self.stream, token, interests)
            .unwrap();
    }
}
