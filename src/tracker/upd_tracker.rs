use std::{
    convert::TryInto,
    error::Error,
    io,
    net::{IpAddr, SocketAddr, UdpSocket},
    time::Duration,
};

use log::{debug, info};
use rand::Rng;
use write_to::{Length, ReadFrom, WriteTo};

use crate::{
    messages::udp_tracker::{
        AnnounceRequest, AnnounceResponse, ConnectRequest, ConnectResponse, ErrorResponse,
    },
    tracker::PeerInfo,
};

use super::{EventKind, TrackerClient, TrackerResponse};

pub struct UdpTracker {
    socket: UdpSocket,
    recv_buffer: Vec<u8>,
}

impl UdpTracker {
    const PROTOCOL_ID: i64 = 0x41727101980;

    pub fn new(address: &str) -> Self {
        let socket = UdpSocket::bind("0.0.0.0:7000").unwrap();
        // remove beginning udp://
        // remove trailing /announce
        let address = address.trim_end_matches("/announce")[6..].to_string();
        // let address = &address[6..].to_string();
        info!("Creating UDP tracker client for {}", address);
        socket.connect(address).unwrap();
        Self {
            socket,
            recv_buffer: Vec::with_capacity(1 << 20),
        }
    }
}

#[allow(dead_code)]
#[derive(Copy, Clone)]
pub enum Action {
    Connect = 0,
    Announce = 1,
    Scrape = 2,
    Error = 3,
}

impl UdpTracker {
    fn send<T: WriteTo + Length + std::fmt::Debug>(&mut self, message: &T) {
        let mut buffer = Vec::new(); // TODO shared buffer
        message.write_to(&mut buffer).unwrap();
        self.socket.send(&buffer).unwrap();
    }

    fn recv<T: ReadFrom + std::fmt::Debug>(&mut self) -> io::Result<T> {
        self.recv_buffer.resize(1 << 16, 0);
        self.socket
            .set_read_timeout(Some(Duration::from_secs(15)))?;
        let length = self.socket.recv(&mut self.recv_buffer)?;
        self.recv_buffer.resize(length, 0);
        let mut slice: &[u8] = &self.recv_buffer;
        let action = u32::from_be_bytes(slice[0..4].try_into().unwrap());
        if action == Action::Error as u32 {
            let (error, _) = ErrorResponse::read_from(&mut slice, length)?;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                String::from_utf8(error.error_string).unwrap(),
            ));
        }
        let (result, length) = T::read_from(&mut slice, length)?;
        assert_eq!(length, 0);
        Ok(result)
    }
}

impl TrackerClient for UdpTracker {
    fn announce(
        &mut self,
        upload: usize,
        download: usize,
        left: usize,
        listen_port: u16,
        info_hash: [u8; 20],
        peer_id: [u8; 20],
        kind: EventKind,
    ) -> Result<TrackerResponse, Box<dyn Error>> {
        let mut rng = rand::thread_rng();
        let transaction_id: i32 = rng.gen();
        debug!("UDP Tracker sending connect request...");
        self.send(&ConnectRequest {
            protocol_id: Self::PROTOCOL_ID,
            action: Action::Connect as _,
            transaction_id,
        });
        let response: ConnectResponse = self.recv()?;
        debug!("UDP Tracker received connect response");
        let transaction_id: i32 = rng.gen();
        let key: u32 = rng.gen(); // no idea
        let request = AnnounceRequest {
            connection_id: response.connection_id,
            action: Action::Announce as _,
            transaction_id,
            info_hash,
            peer_id,
            downloaded: download as _,
            left: left as _,
            uploaded: upload as _,
            event: kind as _,
            ip_address: 0, // Default
            key,           // Wtf is this?  "Randomized by the client", every announce or persisted?
            num_want: -1,  // Default
            port: listen_port,
        };
        debug!("UDP Tracker sending announce request...");
        self.send(&request);
        let AnnounceResponse {
            interval, peers, ..
        } = self.recv()?;
        debug!("UDP Tracker received announce request...");
        let peer_list = peers
            .iter()
            .map(|e| {
                PeerInfo::new_from_addr(SocketAddr::new(IpAddr::V4((e.ip as u32).into()), e.port))
            })
            .collect();
        Ok(TrackerResponse {
            peer_list,
            interval: interval as _,
        })
    }
}
