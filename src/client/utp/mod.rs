pub mod header;
pub use header::*;

pub mod incoming;
pub use incoming::*;

pub mod outgoing;
pub use outgoing::*;

pub mod socket;
pub use socket::*;

pub mod established;
pub use established::*;

use crate::messages::Handshake;

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
    pub fn promote(self) -> UtpSocket {
        match self {
            UtpConnection::Incoming(conn) => conn.promote(),
            UtpConnection::Outgoing(conn) => conn.promote(),
            UtpConnection::Established(_) => panic!(),
        }
    }
}
