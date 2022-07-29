use mio::Poll;

use super::EstablishedConnection;
use super::HandshakingConnection;
use crate::common::Sha1Hash;

pub enum Connection {
    Handshaking(HandshakingConnection),
    Established(EstablishedConnection),
}

impl Connection {
    pub fn deregister(&mut self, poll: &mut Poll) {
        match self {
            Connection::Handshaking(c) => c.deregister(poll),
            Connection::Established(c) => c.deregister(poll),
        }
    }
}

pub trait ConnectionBase: Sized {
    type UpdateSuccessType: Default;

    fn deregister(&mut self, poll: &mut Poll);

    fn update(&mut self) -> Result<Self::UpdateSuccessType, UpdateError>;
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
    UnknownMessage { id: i8 },
    IndexOutOfBounds,
    TorrentNotManaged { info_hash: Sha1Hash },
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
