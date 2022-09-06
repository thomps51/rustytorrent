use std::error::Error;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

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
        &mut self,
        _upload: usize,
        _download: usize,
        _left: usize,
        _listen_port: u16,
        _info_hash_uri: [u8; 20],
        _peer_id: [u8; 20],
        _kind: EventKind,
    ) -> Result<TrackerResponse, Box<dyn Error>> {
        Ok(self.response.clone())
    }
}
