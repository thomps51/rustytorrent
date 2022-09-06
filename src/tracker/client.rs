use std::error::Error;
use std::net::SocketAddr;

use crate::common::{Sha1Hash, PEER_ID_LENGTH};

pub trait TrackerClient {
    fn announce(
        &mut self,
        upload: usize,
        download: usize,
        left: usize,
        listen_port: u16,
        info_hash: Sha1Hash,
        peer_id: [u8; PEER_ID_LENGTH],
        kind: EventKind,
    ) -> Result<TrackerResponse, Box<dyn Error>>;
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub addr: SocketAddr,
    pub id: Option<[u8; PEER_ID_LENGTH]>,
}

impl PeerInfo {
    pub fn new_from_addr(addr: SocketAddr) -> Self {
        Self { addr, id: None }
    }
}

pub type PeerInfoList = Vec<PeerInfo>;

#[derive(Debug, Clone)]
pub struct TrackerResponse {
    pub peer_list: PeerInfoList,
    pub interval: i64,
}

#[derive(Debug)]
pub enum EventKind {
    Started,
    Completed,
    Stopped,
    Empty,
}

impl EventKind {
    pub fn to_str(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Stopped => "stopped",
            Self::Empty => "empty",
        }
    }
}
