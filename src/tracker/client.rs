use std::io::Write;
use std::net::SocketAddr;
use std::{error::Error, io::Read};

use write_to::{ReadFrom, WriteTo};

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

#[derive(Debug, Clone, WriteTo, ReadFrom)]
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

#[repr(u8)]
#[derive(Debug, Clone)]
pub enum EventKind {
    Started = 0,
    Completed,
    Stopped,
    Empty,
}

impl ReadFrom for EventKind {
    fn read_from<T: Read>(reader: &mut T, length: usize) -> std::io::Result<(Self, usize)> {
        let mut buffer = [0; 1];
        reader.read_exact(&mut buffer)?;
        if buffer[0] > 3 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let result: EventKind = unsafe { std::mem::transmute(buffer[0] as u8) };
        Ok((result, length - 1))
    }
}

impl WriteTo for EventKind {
    fn write_to<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        let value = *self as u8;
        writer.write_all(&[value])?;
        Ok(())
    }
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
