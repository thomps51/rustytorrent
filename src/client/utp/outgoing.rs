use crate::client::utp::socket::UtpSocket;
use crate::client::utp::{HandshakingUpdateSuccess, Type};
use crate::client::UpdateError;
use crate::common::Sha1Hash;
use crate::{common::PEER_ID_LENGTH, messages::Handshake};
use log::debug;

use super::Header;

#[derive(Debug)]
pub enum OutgoingUtpState {
    CsSynSent,
    HandshakeSent,
    HandshakeAcked,
}

pub struct OutgoingUtpConnection {
    socket: UtpSocket,
    state: OutgoingUtpState,
    info_hash: Sha1Hash,
    peer_id: [u8; PEER_ID_LENGTH],
    send_buffer: Vec<u8>,
}

impl OutgoingUtpConnection {
    pub fn new(
        mut socket: UtpSocket,
        info_hash: Sha1Hash,
        peer_id: [u8; PEER_ID_LENGTH],
    ) -> std::io::Result<Self> {
        debug!("Creaing OutgoingUtpConnection of type");
        // send initial packet
        socket.send_syn()?;
        Ok(Self {
            socket,
            state: OutgoingUtpState::CsSynSent,
            info_hash,
            send_buffer: Vec::new(),
            peer_id,
        })
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
                let handshake_to_peer = Handshake::new(self.peer_id, &self.info_hash);
                handshake_to_peer.write_to(&mut self.send_buffer).unwrap();
                self.socket.write_buf(&self.send_buffer).unwrap();
                self.send_buffer.clear();
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
