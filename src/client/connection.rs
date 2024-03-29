use crate::common::Sha1Hash;

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
    GeneralError(anyhow::Error),
    CommunicationError(std::io::Error),
    UnknownMessage { id: u8 },
    IndexOutOfBounds,
    TorrentNotManaged { info_hash: Sha1Hash },
    PeerRemoved,
}

impl std::error::Error for UpdateError {}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&format!("{self:?}"))
    }
}

impl From<std::io::Error> for UpdateError {
    fn from(error: std::io::Error) -> Self {
        UpdateError::CommunicationError(error)
    }
}

impl From<std::io::ErrorKind> for UpdateError {
    fn from(error: std::io::ErrorKind) -> Self {
        UpdateError::CommunicationError(error.into())
    }
}

impl From<&std::io::Error> for UpdateError {
    fn from(error: &std::io::Error) -> Self {
        UpdateError::GeneralError(anyhow::Error::msg(format!("{error:?}")))
    }
}

impl From<anyhow::Error> for UpdateError {
    fn from(error: anyhow::Error) -> Self {
        UpdateError::GeneralError(error)
    }
}
