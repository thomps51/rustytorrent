use log::debug;

use crate::{
    common::{Sha1Hash, PEER_ID_LENGTH},
    tracker::{
        EventKind, PeerInfoList, TestTrackerClient, TrackerClient, TrackerClientImpl,
        TrackerResponse,
    },
};
use std::{
    collections::HashMap,
    convert::TryInto,
    sync::mpsc::{Receiver, Sender},
    time::{Duration, Instant},
};

use super::controller::ControllerInputMessage;

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
        info_hash_uri: String,
        peer_id: [u8; PEER_ID_LENGTH],
        listen_port: u16,
        announce_url: String,
    },
    Stop,
}

#[derive(Debug)]
pub enum TrackerReponse {
    Announce {
        info_hash: Sha1Hash,
        peer_list: PeerInfoList,
    },
}

struct TrackerData {
    interval: Option<Duration>,
    last_announce: Option<Instant>,
    tracker: Box<dyn TrackerClient>,
    send: Sender<ControllerInputMessage>,
}

fn create_tracker(announce: &str) -> Box<dyn TrackerClient> {
    const TEST_TRACKER: &'static str = "TestTrackerSelf";
    if announce.starts_with(TEST_TRACKER) {
        let port_str = &announce[TEST_TRACKER.len() + 1..];
        log::debug!("parsing {} into port for TestTrackerSelf", port_str);
        let port: usize = port_str.parse().unwrap();
        Box::new(TestTrackerClient::new_local(port as u16))
    } else if announce.starts_with("None") {
        Box::new(TestTrackerClient::new_empty())
    } else {
        Box::new(TrackerClientImpl {
            address: announce.to_string(),
        })
    }
}

impl TrackerData {
    pub fn new(announce: String, send: Sender<ControllerInputMessage>) -> Self {
        Self {
            tracker: create_tracker(&announce),
            interval: None,
            last_announce: None,
            send,
        }
    }

    pub fn announce(
        &mut self,
        upload: usize,
        download: usize,
        left: usize,
        event: EventKind,
        listen_port: u16,
        info_hash_uri: &str,
        peer_id: &str,
        info_hash: Sha1Hash,
    ) {
        match self.tracker.announce(
            upload,
            download,
            left,
            listen_port,
            info_hash_uri,
            peer_id,
            event,
        ) {
            Ok(TrackerResponse {
                peer_list,
                interval,
            }) => {
                debug!("TrackerResponse: {:?}", peer_list);
                self.interval = Some(Duration::from_secs(interval.try_into().unwrap()));
                self.last_announce = Some(Instant::now());
                self.send
                    .send(ControllerInputMessage::Tracker(TrackerReponse::Announce {
                        info_hash,
                        peer_list,
                    }))
                    .unwrap()
            }
            Err(_) => todo!(),
        }
    }
}

struct TrackersData {
    listen_port: u16,
    info_hash_uri: String,
    info_hash: Sha1Hash,
    peer_id: String,
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
            &self.info_hash_uri,
            &self.peer_id,
            self.info_hash,
        )
    }
}

pub struct TrackerManager {
    torrents: HashMap<Sha1Hash, TrackersData>,
    send: Sender<ControllerInputMessage>,
    recv: Receiver<TrackerRequest>,
}

impl TrackerManager {
    pub fn new(recv: Receiver<TrackerRequest>, send: Sender<ControllerInputMessage>) -> Self {
        Self {
            torrents: HashMap::new(),
            send,
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
                    info_hash_uri,
                    listen_port,
                    peer_id,
                } => self
                    .torrents
                    .entry(info_hash)
                    .or_insert(TrackersData {
                        listen_port,
                        info_hash_uri,
                        trackers: Vec::new(),
                        peer_id: unsafe { String::from_utf8_unchecked(peer_id.to_vec()) },
                        info_hash,
                    })
                    .trackers
                    .push(TrackerData::new(announce_url, self.send.clone())),
                TrackerRequest::Stop => break,
            }
        }
    }
}
