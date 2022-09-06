use log::{debug, info};
use mio::{net::TcpListener, Interest, Poll, Token};

use crate::common::{Sha1Hash, Torrent};

use super::{
    connection_manager::{ConnectionManager, ConnectionManagerConfig},
    disk_manager::{DiskManager, DiskRequest, DiskResponse},
    network_poller_manager::{NetworkPollerManager, PollerRequest, PollerResponse},
    tracker_manager::{TrackerManager, TrackerReponse, TrackerRequest},
};
use std::{
    collections::HashMap,
    sync::mpsc::{channel, Receiver, Sender},
    thread::{self, JoinHandle},
};

const LISTENER: Token = Token(std::usize::MAX - 1); // Max is reserved by impl
pub type CompletionHandler = Sender<bool>;

#[derive(Debug, Clone)]
pub struct ControllerConfig {
    pub listen_port: u16,
    pub max_peers: usize,
    pub seed: bool,
    pub print_output: bool,
}

pub struct Controller {
    send: Sender<ControllerInputMessage>,
    recv: Receiver<ControllerInputMessage>,
    disk_thread: Option<JoinHandle<()>>,
    disk_sender: Sender<DiskRequest>,
    poll_thread: Option<JoinHandle<()>>,
    poll_sender: Sender<PollerRequest>,
    tracker_thread: Option<JoinHandle<()>>,
    tracker_sender: Sender<TrackerRequest>,
    completetion_handlers: HashMap<Sha1Hash, CompletionHandler>,
    connection_manager: ConnectionManager,
}

pub enum ControllerInputMessage {
    Network(PollerResponse),
    Disk(DiskResponse),
    Tracker(TrackerReponse),
    Stop,
}

impl Controller {
    pub fn new_from_channel(
        config: ControllerConfig,
        send: Sender<ControllerInputMessage>,
        recv: Receiver<ControllerInputMessage>,
    ) -> Self {
        debug!("Creating controller");
        let (poll_sender, poll_recv) = channel();
        let mut listener = TcpListener::bind(
            format!("0.0.0.0:{}", config.listen_port)
                .as_str()
                .parse()
                .unwrap(),
        )
        .unwrap();
        let poll = Poll::new().unwrap();
        poll.registry()
            .register(&mut listener, LISTENER, Interest::READABLE)
            .unwrap();
        let poll_thread = {
            let send = send.clone();
            Some(thread::spawn(move || {
                let mut manager = NetworkPollerManager {
                    poll,
                    recv: poll_recv,
                    sender: send,
                };
                manager.run();
            }))
        };

        let (disk_sender, disk_recv) = channel();
        let disk_thread = {
            let send = send.clone();
            Some(thread::spawn(move || {
                let mut manager = DiskManager::new(disk_recv, send);
                manager.run();
            }))
        };
        let (tracker_sender, tracker_recv) = channel();
        let tracker_thread = {
            let send = send.clone();
            Some(thread::spawn(move || {
                let manager = TrackerManager::new(tracker_recv, send);
                manager.run();
            }))
        };
        Self {
            send,
            recv,
            disk_thread,
            disk_sender,
            poll_thread,
            poll_sender: poll_sender.clone(),
            completetion_handlers: HashMap::new(),
            connection_manager: ConnectionManager::new(
                ConnectionManagerConfig {
                    listen_port: config.listen_port,
                    max_peers: config.max_peers,
                    seed: config.seed,
                    print_output: config.print_output,
                },
                poll_sender,
                listener,
                tracker_sender.clone(),
            ),
            tracker_thread,
            tracker_sender,
        }
    }

    pub fn new(config: ControllerConfig) -> Self {
        let (send, recv) = channel();
        Self::new_from_channel(config, send, recv)
    }

    pub fn add_torrent(&mut self, torrent: Torrent, completion_handler: Option<CompletionHandler>) {
        // debug!("Adding torrent: {:?}", torrent);
        if let Some(handler) = completion_handler {
            self.completetion_handlers
                .insert(torrent.metainfo.info_hash_raw, handler);
        }
        self.disk_sender
            .send(DiskRequest::AddTorrent {
                meta_info: torrent.metainfo,
                destination: torrent.destination,
            })
            .unwrap();
    }

    pub fn get_sender(&self) -> Sender<ControllerInputMessage> {
        self.send.clone()
    }

    pub fn handle_disk_message(&mut self, message: DiskResponse) {
        match message {
            DiskResponse::AddTorrentCompleted {
                meta_info,
                piece_have,
            } => {
                self.connection_manager.start_torrent(
                    meta_info,
                    piece_have,
                    self.disk_sender.clone(),
                );
            }
            DiskResponse::RequestCompleted {
                info_hash,
                token,
                block,
            } => {
                self.connection_manager.send_block(info_hash, token, block);
            }
            DiskResponse::WritePieceCompleted {
                info_hash,
                piece_index,
                final_piece,
            } => {
                self.connection_manager.send_have(info_hash, piece_index);
                if final_piece {
                    if let Some(handler) = self.completetion_handlers.get(&info_hash) {
                        handler.send(true).unwrap();
                    }
                }
            }
            DiskResponse::Error { .. } => unimplemented!(),
        }
    }

    pub fn handle_poll_message(&mut self, message: PollerResponse) {
        match message {
            PollerResponse::Events { events } => {
                for event in &events {
                    // if event is_writable, that means it is an outgoing connection that is now connected
                    if event.is_writable() {
                        self.connection_manager.reregister_connected(event.token());
                        continue;
                    }
                    match event.token() {
                        LISTENER => self.connection_manager.accept_connections(),
                        token => self.connection_manager.handle_event(token),
                    }
                }
            }
            PollerResponse::RegisterDone {
                info_hash,
                source,
                token,
                interests,
                peer_info,
            } => {
                // Register could be done on either:
                // Incoming connection that was accepted
                // Outgoing connection if is_writable
                if interests.is_writable() {
                    self.connection_manager.add_outgoing_connection(
                        source,
                        token,
                        peer_info,
                        info_hash.unwrap(),
                    )
                } else {
                    self.connection_manager
                        .add_incoming_connection(source, token, peer_info)
                }
            }
            PollerResponse::ReregisterDone {
                info_hash,
                source,
                token,
                ..
            } => {
                self.connection_manager.start_outgoing_connection(
                    info_hash.unwrap(),
                    source,
                    token,
                );
            }
            PollerResponse::DeregisterDone { .. } => {}
        }
    }

    fn handle_tracker_message(&mut self, message: TrackerReponse) {
        debug!("Tracker message Received: {:?}", message);
        match message {
            TrackerReponse::Announce {
                info_hash,
                peer_list,
            } => self.connection_manager.add_peers(info_hash, peer_list),
        }
    }

    pub fn run(mut self) {
        debug!("Running controller");
        loop {
            match self.recv.recv().unwrap() {
                ControllerInputMessage::Disk(message) => self.handle_disk_message(message),
                ControllerInputMessage::Network(message) => self.handle_poll_message(message),
                ControllerInputMessage::Tracker(message) => self.handle_tracker_message(message),
                ControllerInputMessage::Stop => break,
            }
            self.connection_manager.maybe_print_info();
        }
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        // info!("{:?}", std::backtrace::Backtrace::capture());
        self.disk_sender.send(DiskRequest::Stop).unwrap();
        self.poll_sender.send(PollerRequest::Stop).unwrap();
        self.tracker_sender.send(TrackerRequest::Stop).unwrap();
        let _ = self.send.send(ControllerInputMessage::Stop); // We don't know if it is stopped
        info!("Sending stop messages and joining threads");
        self.disk_thread.take().unwrap().join().unwrap();
        self.poll_thread.take().unwrap().join().unwrap();
        self.tracker_thread.take().unwrap().join().unwrap();
        info!("Done joining threads");
    }
}
