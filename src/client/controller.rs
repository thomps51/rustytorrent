use log::{debug, info};
use mio::{net::TcpListener, net::UdpSocket, Events, Interest, Poll, Token};
use rccell::RcCell;

use crate::common::{new_udp_socket, Sha1Hash, Torrent};

use super::{
    connection_manager::{ConnectionManager, ConnectionManagerConfig},
    disk_manager::{DiskManager, DiskRequest, DiskResponse},
    tracker_manager::{TrackerManager, TrackerRequest, TrackerThreadResponse},
};
use std::{
    collections::HashMap,
    sync::mpsc::{channel, Receiver, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

const TCP_LISTENER: Token = Token(std::usize::MAX - 1); // Max is reserved by impl
const DISK_MESSAGE: Token = Token(std::usize::MAX - 2);
const TRACKER_MESSAGE: Token = Token(std::usize::MAX - 3);
const STOP_MESSAGE: Token = Token(std::usize::MAX - 4);
const UTP_SOCKET: Token = Token(std::usize::MAX - 5);
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
    disk_message_socket: UdpSocket,
    tracker_message_socket: UdpSocket,
    stop_message_socket: UdpSocket,
    disk_thread: Option<JoinHandle<()>>,
    disk_sender: Sender<DiskRequest>,
    disk_recv: Receiver<DiskResponse>,
    tracker_thread: Option<JoinHandle<()>>,
    tracker_sender: Sender<TrackerRequest>,
    tracker_recv: Receiver<TrackerThreadResponse>,
    stop_thread: Option<JoinHandle<()>>,
    completetion_handlers: HashMap<Sha1Hash, CompletionHandler>,
    connection_manager: ConnectionManager,
    poll: RcCell<Poll>,
}

pub enum ControllerInputMessage {
    Disk(DiskResponse),
    Tracker(TrackerThreadResponse),
    Stop,
}

impl Controller {
    pub fn new_from_channel(
        config: ControllerConfig,
        send: Sender<ControllerInputMessage>,
        recv: Receiver<ControllerInputMessage>,
    ) -> Self {
        debug!("Creating controller");
        let mut listener = TcpListener::bind(
            format!("0.0.0.0:{}", config.listen_port)
                .as_str()
                .parse()
                .unwrap(),
        )
        .unwrap();
        let mut disk_message_socket = new_udp_socket();
        let disk_message_socket_addr = disk_message_socket.local_addr().unwrap();
        let mut tracker_message_socket = new_udp_socket();
        let tracker_message_socket_addr = tracker_message_socket.local_addr().unwrap();
        let mut stop_message_recv_socket = new_udp_socket();
        let stop_message_socket_addr = stop_message_recv_socket.local_addr().unwrap();
        let mut utp_socket = UdpSocket::bind(
            format!("0.0.0.0:{}", config.listen_port)
                .as_str()
                .parse()
                .unwrap(),
        )
        .unwrap();
        let poll = Poll::new().unwrap();
        poll.registry()
            .register(&mut listener, TCP_LISTENER, Interest::READABLE)
            .unwrap();
        poll.registry()
            .register(&mut utp_socket, UTP_SOCKET, Interest::READABLE)
            .unwrap();
        poll.registry()
            .register(&mut disk_message_socket, DISK_MESSAGE, Interest::READABLE)
            .unwrap();
        poll.registry()
            .register(
                &mut tracker_message_socket,
                TRACKER_MESSAGE,
                Interest::READABLE,
            )
            .unwrap();
        poll.registry()
            .register(
                &mut stop_message_recv_socket,
                STOP_MESSAGE,
                Interest::READABLE,
            )
            .unwrap();
        let poll = RcCell::new(poll);

        let (disk_request_sender, disk_request_recv) = channel();
        let (disk_response_sender, disk_response_recv) = channel();
        let disk_thread = {
            Some(thread::spawn(move || {
                let mut manager = DiskManager::new(
                    disk_request_recv,
                    disk_response_sender,
                    disk_message_socket_addr,
                );
                manager.run();
            }))
        };
        let (tracker_request_sender, tracker_request_recv) = channel();
        let (tracker_response_sender, tracker_response_recv) = channel();
        let tracker_thread = {
            Some(thread::spawn(move || {
                let manager = TrackerManager::new(
                    tracker_request_recv,
                    tracker_response_sender,
                    tracker_message_socket_addr,
                );
                manager.run();
            }))
        };
        let stop_thread = {
            Some(thread::spawn(move || {
                let socket = new_udp_socket();
                socket.connect(stop_message_socket_addr).unwrap();
                if recv.recv().is_ok() {
                    socket.send(&[1]).unwrap();
                }
            }))
        };
        Self {
            send,
            disk_thread,
            disk_sender: disk_request_sender,
            disk_recv: disk_response_recv,
            completetion_handlers: HashMap::new(),
            connection_manager: ConnectionManager::new(
                ConnectionManagerConfig {
                    listen_port: config.listen_port,
                    max_peers: config.max_peers,
                    seed: config.seed,
                    print_output: config.print_output,
                },
                poll.clone(),
                listener,
                utp_socket,
                tracker_request_sender.clone(),
            ),
            tracker_thread,
            tracker_sender: tracker_request_sender,
            tracker_recv: tracker_response_recv,
            disk_message_socket,
            tracker_message_socket,
            stop_message_socket: stop_message_recv_socket,
            stop_thread,
            poll,
        }
    }

    pub fn new(config: ControllerConfig) -> Self {
        let (send, recv) = channel();
        Self::new_from_channel(config, send, recv)
    }

    pub fn add_torrent(&mut self, torrent: Torrent, completion_handler: Option<CompletionHandler>) {
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

    pub fn handle_disk_message(&mut self) {
        while let Ok(message) = self.disk_recv.try_recv() {
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
                    conn_id,
                    block,
                } => {
                    self.connection_manager
                        .send_block(info_hash, conn_id, block);
                }
                DiskResponse::WritePieceCompleted {
                    info_hash,
                    piece_index,
                    final_piece,
                } => {
                    debug!(
                        "Write piece completed: piece_index: {}, final_piece: {}",
                        piece_index, final_piece
                    );
                    self.connection_manager.send_have(info_hash, piece_index);
                    if final_piece {
                        if let Some(handler) = self.completetion_handlers.get(&info_hash) {
                            handler.send(true).unwrap();
                        }
                    }
                    // If all of the write pieces come back after we have exhausted our assigned blocks,
                    // we need to make sure to continue updating
                }
                DiskResponse::Error { .. } => unimplemented!(),
                DiskResponse::WritePieceFailedHash {
                    info_hash: _,
                    piece_index: _,
                } => todo!(),
            }
            Self::clear_internal_message(&self.disk_message_socket);
        }
    }

    fn handle_tracker_message(&mut self) {
        while let Ok(message) = self.tracker_recv.try_recv() {
            debug!("Tracker message Received: {:?}", message);
            match message {
                TrackerThreadResponse::Announce {
                    info_hash,
                    peer_list,
                } => self.connection_manager.add_peers(info_hash, peer_list),
            }
            Self::clear_internal_message(&self.tracker_message_socket);
        }
    }

    fn clear_internal_message(socket: &UdpSocket) {
        let mut buf = [0; 1];
        let _ = socket.recv(&mut buf).unwrap();
    }

    pub fn run(mut self) {
        debug!("Running controller");
        let mut events = Events::with_capacity(1024);
        'outer: loop {
            self.poll
                .borrow_mut()
                .poll(&mut events, Some(Duration::from_secs(1)))
                .unwrap();
            if events.is_empty() {
                debug!("no events!");
            } else {
                debug!("events: {:?}", events);
            }
            for event in &events {
                match event.token() {
                    TCP_LISTENER => self.connection_manager.accept_tcp_connections(),
                    DISK_MESSAGE => self.handle_disk_message(),
                    TRACKER_MESSAGE => self.handle_tracker_message(),
                    STOP_MESSAGE => break 'outer,
                    UTP_SOCKET => self.connection_manager.handle_utp_event(),
                    token => self.connection_manager.handle_event(token),
                }
            }
            self.connection_manager.maybe_print_info();
        }
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        // info!("{:?}", std::backtrace::Backtrace::capture());
        self.disk_sender.send(DiskRequest::Stop).unwrap();
        self.tracker_sender.send(TrackerRequest::Stop).unwrap();
        let _ = self.send.send(ControllerInputMessage::Stop); // We don't know if it is stopped
        let _ = self.stop_message_socket.send(&[1]);
        info!("Sending stop messages and joining threads");
        self.disk_thread.take().unwrap().join().unwrap();
        self.tracker_thread.take().unwrap().join().unwrap();
        self.stop_thread.take().unwrap().join().unwrap();
        info!("Done joining threads");
    }
}
