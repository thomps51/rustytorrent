use super::EstablishedConnection;
use super::HandshakingConnection;
use super::NetworkSource;
use crate::common::Sha1Hash;

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

    fn update(&mut self) -> Result<Self::UpdateSuccessType, UpdateError>;

    fn into_network_source(self) -> NetworkSource;
}

pub type UpdateResult = Result<UpdateSuccess, UpdateError>;

#[derive(Debug, Default)]
pub enum UpdateSuccess {
    #[default]
    NoUpdate,
    Transferred {
        downloaded: usize,
        uploaded: usize,
    },
    Success,
}

#[derive(Debug)]
pub enum UpdateError {
    CommunicationError(std::io::Error),
    UnknownMessage { id: u8 },
    IndexOutOfBounds,
    TorrentNotManaged { info_hash: Sha1Hash },
    PeerRemoved,
}

impl std::error::Error for UpdateError {}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&format!("{:?}", self))
    }
}

impl From<std::io::Error> for UpdateError {
    fn from(error: std::io::Error) -> Self {
        UpdateError::CommunicationError(error)
    }
}
