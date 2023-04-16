use crate::client::utp::socket::UtpConnectionInfo;
use crate::client::utp::Type;
use crate::client::UpdateError;
use crate::{common::PEER_ID_LENGTH, messages::Handshake};
use log::debug;

use super::{HandshakingUpdateSuccess, Header, UtpSendBuffer};

#[derive(Debug)]
pub enum IncomingUtpState {
    CsSynRecv,
    CsStateSent,
}

pub struct IncomingUtpConnection {
    pub(crate) socket: UtpConnectionInfo,
    state: IncomingUtpState,
    peer_id: [u8; PEER_ID_LENGTH],
}

impl IncomingUtpConnection {
    pub fn new(socket: UtpConnectionInfo, peer_id: [u8; PEER_ID_LENGTH]) -> Self {
        IncomingUtpConnection {
            socket,
            state: IncomingUtpState::CsSynRecv,
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
            IncomingUtpState::CsSynRecv => {
                send_buffer.add_header(Type::StState);
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
                // If the handshake is in the same STDATA message as other BT messages, this will send an ACK twice? (also in Established)
                // Does outgoing send the handshake with other data?
                send_buffer.add_header(Type::StState);
                send_buffer.add_data(Handshake::new(self.peer_id, &handshake_from_peer.info_hash));
                Ok(HandshakingUpdateSuccess::Complete(handshake_from_peer))
            }
        }
    }
}
