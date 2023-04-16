use crate::client::utp::connection_info::UtpConnectionInfo;
use crate::client::utp::{HandshakingUpdateSuccess, Type};
use crate::client::UpdateError;
use crate::common::Sha1Hash;
use crate::{common::PEER_ID_LENGTH, messages::Handshake};
use log::debug;

use super::{Header, UtpSendBuffer};

#[derive(Debug)]
pub enum OutgoingUtpState {
    CsSynSent,
    HandshakeSent,
    HandshakeAcked,
}

pub struct OutgoingUtpConnection {
    pub(crate) socket: UtpConnectionInfo,
    state: OutgoingUtpState,
    info_hash: Sha1Hash,
    peer_id: [u8; PEER_ID_LENGTH],
}

impl OutgoingUtpConnection {
    pub fn new(
        socket: UtpConnectionInfo,
        info_hash: Sha1Hash,
        peer_id: [u8; PEER_ID_LENGTH],
    ) -> Self {
        Self {
            socket,
            state: OutgoingUtpState::CsSynSent,
            info_hash,
            peer_id,
        }
    }

    pub fn promote(self) -> UtpConnectionInfo {
        self.socket
    }

    pub fn update(
        &mut self,
        read_buffer: &mut crate::io::ReadBuffer,
        header: &Header,
        send_buffer: &mut UtpSendBuffer,
    ) -> Result<HandshakingUpdateSuccess, UpdateError> {
        self.socket.process_header(header);
        debug!("update of state: {:?}", self.state);
        match self.state {
            OutgoingUtpState::CsSynSent => {
                // TODO: Split these into Incoming/Outgoing classes?
                // Sent initiating packet, wait until we recv ST_STATE packet
                if header.get_type() != Type::StState {
                    if header.get_type() == Type::StSyn {
                        log::debug!("detected self connection");
                    }
                    log::warn!(
                        "Unexpected header type in state CS_SYN_SENT from {:?}: {:?}",
                        self.socket.addr(),
                        header.get_type()
                    );
                    return Err(crate::client::UpdateError::CommunicationError(
                        std::io::ErrorKind::InvalidData.into(),
                    ));
                }
                // Now send handshake, wait for handshake from peer
                self.state = OutgoingUtpState::HandshakeSent;
                self.socket.ack_nr = header.seq_nr - 1; // Handshake ack_nr seems to be SEQ_NR - 1
                debug!("Outgoing connection, sending handshake");
                send_buffer.add_data(Handshake::new(self.peer_id, &self.info_hash));
                Ok(HandshakingUpdateSuccess::NoUpdate)
            }
            OutgoingUtpState::HandshakeSent => {
                if header.get_type() != Type::StState {
                    log::warn!(
                        "Unexpected header type in state HandshakeSent from {:?}: {:?}",
                        self.socket.addr(),
                        header.get_type()
                    );
                    return Err(crate::client::UpdateError::CommunicationError(
                        std::io::ErrorKind::InvalidData.into(),
                    ));
                }
                debug!("Outgoing connnection, handshake Acked");
                self.state = OutgoingUtpState::HandshakeAcked;
                Ok(HandshakingUpdateSuccess::NoUpdate)
            }
            OutgoingUtpState::HandshakeAcked => {
                // TODO: why am I getting a double ACK herre in testing?
                if header.get_type() == Type::StState {
                    return Ok(HandshakingUpdateSuccess::NoUpdate);
                }
                if header.get_type() != Type::StData {
                    log::warn!(
                        "Unexpected header type in state HandShakeAcked from {:?}: {:?}",
                        self.socket.addr(),
                        header.get_type()
                    );
                    return Err(crate::client::UpdateError::CommunicationError(
                        std::io::ErrorKind::InvalidData.into(),
                    ));
                }
                // I will likely receive an ACK before the handshake
                let handshake_from_peer = Handshake::read_from(read_buffer)?;
                // may have more than just handshake data, including have info, unchoke, etc
                // debug!("Got handshake from peer {}", self.token.0);
                debug!("Outgoing utp got handshake from peer");
                Ok(HandshakingUpdateSuccess::Complete(handshake_from_peer))
            }
        }
    }
}
