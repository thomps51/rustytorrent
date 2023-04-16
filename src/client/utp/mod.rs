pub mod header;
pub use header::*;

pub mod incoming;
pub use incoming::*;

pub mod outgoing;
pub use outgoing::*;

pub mod connection_info;
pub use connection_info::*;

pub mod established;
pub use established::*;

pub mod send_buffer;
pub use send_buffer::*;

pub mod connection_manager;

use crate::{io::ReadBuffer, messages::Handshake};

use super::{UpdateError, UpdateSuccess};

#[derive(Default)]
pub enum HandshakingUpdateSuccess {
    #[default]
    NoUpdate,
    Complete(Handshake),
}

pub enum UtpConnection {
    Incoming(IncomingUtpConnection),
    Outgoing(OutgoingUtpConnection),
    Established(EstablishedUtpConnection),
}

impl UtpConnection {
    pub fn promote(self) -> UtpConnectionInfo {
        match self {
            UtpConnection::Incoming(conn) => conn.promote(),
            UtpConnection::Outgoing(conn) => conn.promote(),
            UtpConnection::Established(_) => panic!(),
        }
    }

    pub fn connection_info(&mut self) -> &mut UtpConnectionInfo {
        match self {
            UtpConnection::Incoming(c) => &mut c.socket,
            UtpConnection::Outgoing(c) => &mut c.socket,
            UtpConnection::Established(c) => &mut c.stream,
        }
    }

    pub fn update(
        &mut self,
        header: &Header,
        read_buffer: &mut ReadBuffer,
        send_buffer: &mut UtpSendBuffer,
    ) -> Result<Option<Handshake>, UpdateError> {
        match self {
            UtpConnection::Incoming(conn) => {
                if let HandshakingUpdateSuccess::Complete(handshake) =
                    conn.update(read_buffer, header, send_buffer)?
                {
                    return Ok(Some(handshake));
                }
            }
            UtpConnection::Outgoing(conn) => {
                if let HandshakingUpdateSuccess::Complete(handshake) =
                    conn.update(read_buffer, header, send_buffer)?
                {
                    return Ok(Some(handshake));
                }
            }
            UtpConnection::Established(conn) => {
                match conn.update(read_buffer, header, send_buffer)? {
                    UpdateSuccess::Transferred {
                        downloaded: _,
                        uploaded: _,
                    } => {
                        // TODO: Propagate this
                        // self.downloaded += downloaded;
                        // self.uploaded += uploaded;
                    }
                    UpdateSuccess::NoUpdate | UpdateSuccess::Success => {}
                }
            }
        }
        Ok(None)
    }
}
