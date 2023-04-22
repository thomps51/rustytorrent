use crate::io::ReadBuffer;

use self::{established::EstablishedConnection, handshaking::HandshakingConnection};

use super::{NetworkSource, UpdateError};

pub mod connection_manager;
pub mod established;
pub mod handshaking;

pub enum Connection {
    Handshaking(HandshakingConnection),
    Established(EstablishedConnection),
}

impl Connection {
    pub fn into_network_source(self) -> NetworkSource {
        match self {
            Connection::Handshaking(c) => c.into_network_source(),
            Connection::Established(c) => c.into_network_source(),
        }
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
