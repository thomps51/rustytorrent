use log::debug;
use mio::net::UdpSocket;

use crate::{
    common::{new_udp_socket, Sha1Hash, PEER_ID_LENGTH},
    tracker::{
        http_tracker::HttpTracker, upd_tracker::UdpTracker, EventKind, PeerInfoList,
        TestTrackerClient, TrackerClient, TrackerResponse,
    },
};
use std::{
    collections::HashMap,
    convert::TryInto,
    net::SocketAddr,
    rc::Rc,
    sync::mpsc::{Receiver, Sender},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum TrackerRequest {
    Announce {
        info_hash: Sha1Hash,
        upload: usize,
        download: usize,
        left: usize,
        event: EventKind,
    },
    Register {
        info_hash: Sha1Hash,
        peer_id: [u8; PEER_ID_LENGTH],
        listen_port: u16,
        announce_url: String,
    },
    Stop,
}

#[derive(Debug)]
pub enum TrackerThreadResponse {
    Announce {
        info_hash: Sha1Hash,
        peer_list: PeerInfoList,
    },
}

struct TrackerData {
    interval: Option<Duration>,
    last_announce: Option<Instant>,
    tracker: Box<dyn TrackerClient>,
    send_socket: Rc<UdpSocket>,
    send_chan: Sender<TrackerThreadResponse>,
}

fn create_tracker(announce: String) -> Box<dyn TrackerClient> {
    const TEST_TRACKER: &str = "TestTrackerSelf";
    if announce.starts_with(TEST_TRACKER) {
        let port_str = &announce[TEST_TRACKER.len() + 1..];
        log::debug!("parsing {} into port for TestTrackerSelf", port_str);
        let port: usize = port_str.parse().unwrap();
        Box::new(TestTrackerClient::new_local(port as u16))
    } else if announce.starts_with("None") {
        Box::new(TestTrackerClient::new_empty())
    } else if announce.starts_with("http") {
        Box::new(HttpTracker { address: announce })
    } else if announce.starts_with("udp") {
        Box::new(UdpTracker::new(&announce))
    } else {
        panic!("Not sure what tracker to use for {announce}");
    }
}

impl TrackerData {
    pub fn new(
        announce: String,
        send: Sender<TrackerThreadResponse>,
        send_socket: Rc<UdpSocket>,
    ) -> Self {
        Self {
            tracker: create_tracker(announce),
            interval: None,
            last_announce: None,
            send_socket,
            send_chan: send,
        }
    }

    pub fn announce(
        &mut self,
        upload: usize,
        download: usize,
        left: usize,
        event: EventKind,
        listen_port: u16,
        info_hash: Sha1Hash,
        peer_id: [u8; PEER_ID_LENGTH],
    ) {
        match self.tracker.announce(
            upload,
            download,
            left,
            listen_port,
            info_hash,
            peer_id,
            event,
        ) {
            Ok(TrackerResponse {
                peer_list,
                interval,
            }) => {
                debug!("TrackerResponse: received");
                self.interval = Some(Duration::from_secs(interval.try_into().unwrap()));
                self.last_announce = Some(Instant::now());
                self.send_chan
                    .send(TrackerThreadResponse::Announce {
                        info_hash,
                        peer_list,
                    })
                    .unwrap();
                self.send_socket.send(&[1]).unwrap();
            }
            Err(error) => panic!("Error while announcing to tracker: {error:?}"),
        }
    }
}

struct TrackersData {
    listen_port: u16,
    info_hash: Sha1Hash,
    peer_id: [u8; PEER_ID_LENGTH],
    trackers: Vec<TrackerData>,
}

impl TrackersData {
    pub fn announce(&mut self, upload: usize, download: usize, left: usize, event: EventKind) {
        // TODO assume we have 1 tracker for now
        self.trackers[0].announce(
            upload,
            download,
            left,
            event,
            self.listen_port,
            self.info_hash,
            self.peer_id,
        )
    }
}

pub struct TrackerManager {
    torrents: HashMap<Sha1Hash, TrackersData>,
    send_socket: Rc<UdpSocket>,
    send_chan: Sender<TrackerThreadResponse>,
    recv: Receiver<TrackerRequest>,
}

impl TrackerManager {
    pub fn new(
        recv: Receiver<TrackerRequest>,
        send: Sender<TrackerThreadResponse>,
        send_socket_addr: SocketAddr,
    ) -> Self {
        let send_socket = new_udp_socket();
        send_socket.connect(send_socket_addr).unwrap();
        Self {
            torrents: HashMap::new(),
            send_socket: send_socket.into(),
            send_chan: send,
            recv,
        }
    }

    pub fn run(mut self) {
        loop {
            let message = self.recv.recv().unwrap();
            debug!("TrackerRequest received: {:?}", message);
            match message {
                TrackerRequest::Announce {
                    info_hash,
                    upload,
                    download,
                    left,
                    event,
                } => {
                    self.torrents
                        .get_mut(&info_hash)
                        .unwrap()
                        .announce(upload, download, left, event);
                }
                TrackerRequest::Register {
                    info_hash,
                    announce_url,
                    listen_port,
                    peer_id,
                } => self
                    .torrents
                    .entry(info_hash)
                    .or_insert(TrackersData {
                        listen_port,
                        info_hash,
                        trackers: Vec::new(),
                        peer_id,
                    })
                    .trackers
                    .push(TrackerData::new(
                        announce_url,
                        self.send_chan.clone(),
                        self.send_socket.clone(),
                    )),
                TrackerRequest::Stop => break,
            }
        }
    }
}
