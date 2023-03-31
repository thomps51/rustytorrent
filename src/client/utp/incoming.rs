use crate::client::utp::socket::UtpSocket;
use crate::client::utp::Type;
use crate::client::UpdateError;
use crate::{common::PEER_ID_LENGTH, messages::Handshake};
use log::debug;

use super::{HandshakingUpdateSuccess, Header};

#[derive(Debug)]
pub enum IncomingUtpState {
    CsSynRecv,
    CsStateSent,
}

pub struct IncomingUtpConnection {
    socket: UtpSocket,
    state: IncomingUtpState,
    peer_id: [u8; PEER_ID_LENGTH],
    send_buffer: Vec<u8>,
}

impl IncomingUtpConnection {
    pub fn new(socket: UtpSocket, peer_id: [u8; PEER_ID_LENGTH]) -> Self {
        IncomingUtpConnection {
            socket,
            state: IncomingUtpState::CsSynRecv,
            peer_id,
            send_buffer: vec![],
        }
    }

    pub fn promote(self) -> UtpSocket {
        self.socket
    }

    pub fn update(
        &mut self,
        read_buffer: &mut crate::io::ReadBuffer,
        header: &Header,
    ) -> Result<HandshakingUpdateSuccess, UpdateError> {
        self.socket.process_header(header);
        debug!("update of state: {:?}", self.state);
        match self.state {
            IncomingUtpState::CsSynRecv => {
                self.socket.send_header(Type::StState)?;
                self.state = IncomingUtpState::CsStateSent;
                Ok(HandshakingUpdateSuccess::NoUpdate)
            }
            IncomingUtpState::CsStateSent => {
                // wait until we receive ST_DATA packet
                // this ST_DATA packet contains the handshake
                if header.get_type() != Type::StData {
                    log::warn!(
                        "Unexpected header type in state CS_STATE_SENT from {:?}: {:?}",
                        self.socket.addr(),
                        header.get_type()
                    );
                    return Err(crate::client::UpdateError::CommunicationError(
                        std::io::ErrorKind::InvalidData.into(),
                    ));
                }
                debug!("promoting incoming connection!");
                let handshake_from_peer = Handshake::read_from(read_buffer)?;
                // ACK the handshake
                self.socket.send_header(Type::StState)?;
                let handshake_to_peer =
                    Handshake::new(self.peer_id, &handshake_from_peer.info_hash);
                handshake_to_peer.write_to(&mut self.send_buffer).unwrap();
                self.socket.write_buf(&self.send_buffer).unwrap();
                self.send_buffer.clear();
                Ok(HandshakingUpdateSuccess::Complete(handshake_from_peer))
            }
        }
    }
}
