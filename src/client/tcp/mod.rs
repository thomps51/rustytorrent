use crate::io::ReadBuffer;

use self::{established::EstablishedConnection, handshaking::HandshakingConnection};

use super::{NetworkSource, UpdateError};

pub mod connection_manager;
pub mod established;
pub mod handshaking;

pub enum Connection {
    Empty,
    Handshaking(HandshakingConnection),
    Established(EstablishedConnection),
}

impl Connection {
    pub fn into_network_source(self) -> NetworkSource {
        match self {
            Connection::Handshaking(c) => c.into_network_source(),
            Connection::Established(c) => c.into_network_source(),
            Connection::Empty => panic!("not implemented for empty socket"),
        }
    }

    pub fn reset(&mut self) {
        *self = Connection::Empty;
    }

    pub fn take(&mut self) -> Connection {
        std::mem::replace(self, Connection::Empty)
    }
}

pub trait ConnectionBase: Sized {
    type UpdateSuccessType: Default;

    fn update(
        &mut self,
        read_buffer: &mut ReadBuffer,
    ) -> Result<Self::UpdateSuccessType, UpdateError>;

    fn into_network_source(self) -> NetworkSource;
}
