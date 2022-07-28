use std::error::Error;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

use crate::common::Torrent;

use super::{EventKind, PeerInfo, TrackerClient, TrackerResponse};

pub struct TestTrackerClient {
    response: TrackerResponse,
}

impl TestTrackerClient {
    pub fn new_local(listen_port: u16) -> Self {
        format!("0.0.0.0:{}", listen_port);
        let ip_address = IpAddr::from_str("0.0.0.0").unwrap();
        let addr = SocketAddr::new(ip_address, listen_port);
        Self {
            response: TrackerResponse {
                peer_list: vec![PeerInfo { addr, id: None }],
                interval: 0,
            },
        }
    }

    pub fn new_empty() -> Self {
        Self {
            response: TrackerResponse {
                peer_list: vec![],
                interval: 0,
            },
        }
    }
}

impl TrackerClient for TestTrackerClient {
    fn announce(
        &self,
        _torrent: &Torrent,
        _kind: EventKind,
    ) -> Result<TrackerResponse, Box<dyn Error>> {
        Ok(self.response.clone())
    }
}
