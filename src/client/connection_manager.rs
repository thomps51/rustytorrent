use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error;
use std::iter::FromIterator;
use std::mem::take;
use std::rc::Rc;
use std::sync::mpsc;

use bit_vec::BitVec;
use log::info;
use mio::event::Event;
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token};

use super::piece_info::PieceInfo;
use super::piece_store::{FileSystem, PieceStore};
use super::{
    CompletionHandler, Connection, EstablishedConnection, HandshakeUpdateSuccess, UpdateSuccess,
};
use super::{HandshakingConnection, PieceAssigner};
use crate::client::connection::ConnectionBase;
use crate::common::SharedPieceStore;
use crate::common::Torrent;
use crate::common::{Sha1Hash, SharedPieceAssigner};
use crate::messages::{Bitfield, Have, Interested, Unchoke};
use crate::tracker::{EventKind, PeerInfo, TrackerClient};

const LISTENER: Token = Token(std::usize::MAX);
const PRINT_UPDATE_TIME: std::time::Duration = std::time::Duration::from_secs(1);

pub struct ConnectionManager<T: TrackerClient> {
    connections: HashMap<Token, Connection>,
    downloaded: usize,
    uploaded: usize,
    last_update: std::time::Instant,
    next_socket_index: usize,
    poll: Poll,
    send_have: mpsc::Sender<Have>,
    recv_have: mpsc::Receiver<Have>,
    torrents: HashMap<Sha1Hash, TorrentData<T>>,
    config: ConnectionManagerConfig,
    stop: bool,
    listener: TcpListener,
    events: Events,
}

pub struct ConnectionManagerConfig {
    pub listen_port: u16,
    pub max_peers: usize,
    pub seed: bool,
    pub print_output: bool,
}

pub struct TorrentData<T: TrackerClient> {
    torrent: Torrent,
    piece_assigner: SharedPieceAssigner,
    piece_store: SharedPieceStore, // This is per torrent right now, but doesn't really scale well since each gets 1 thread
    downloaded: usize,
    uploaded: usize,
    connections: Vec<Token>,
    trackers: Vec<T>,
    peer_list: Vec<PeerInfo>,
    piece_info: PieceInfo,
}

impl<T: TrackerClient> ConnectionManager<T> {
    pub fn new(config: ConnectionManagerConfig) -> Self {
        let (send_have, recv_have) = mpsc::channel();
        let poll = Poll::new().unwrap();
        let listener = TcpListener::bind(
            format!("0.0.0.0:{}", config.listen_port)
                .as_str()
                .parse()
                .unwrap(),
        )
        .unwrap();
        let events = Events::with_capacity(1024);
        ConnectionManager {
            connections: HashMap::new(),
            downloaded: 0,
            uploaded: 0,
            last_update: std::time::Instant::now(),
            next_socket_index: 0,
            poll,
            recv_have,
            send_have,
            torrents: HashMap::new(),
            config,
            stop: false,
            listener,
            events,
        }
    }

    pub fn add_torrent(
        &mut self,
        torrent: Torrent,
        tracker: T,
        completion_handler: Option<CompletionHandler>,
    ) -> Result<(), Box<dyn Error>> {
        let num_pieces = torrent.metainfo.pieces.len();
        let piece_length = torrent.metainfo.piece_length;
        let (send, recv) = mpsc::channel();
        info!("Piece length: {}, Num pieces: {}", piece_length, num_pieces);
        let piece_info = PieceInfo {
            piece_length,
            total_pieces: num_pieces,
            total_length: torrent.metainfo.total_size,
        };
        let piece_store: Rc<RefCell<_>> = Rc::new(RefCell::new(
            FileSystem::new(
                &torrent,
                piece_info,
                send,
                self.send_have.clone(),
                completion_handler,
            )
            .unwrap(),
        ));
        let piece_assigner = Rc::new(RefCell::new(PieceAssigner::new(
            piece_info,
            recv,
            piece_store.borrow().have(),
        )));
        let response = tracker.announce(&torrent, EventKind::Started)?;
        let peer_list = response.peer_list;
        self.torrents.insert(
            torrent.metainfo.info_hash_raw,
            TorrentData {
                torrent,
                piece_assigner,
                piece_store,
                downloaded: 0,
                uploaded: 0,
                connections: Vec::new(),
                trackers: vec![tracker],
                peer_list,
                piece_info,
            },
        );
        Ok(())
    }

    pub fn start(&mut self) -> Result<(), Box<dyn Error>> {
        self.poll
            .registry()
            .register(&mut self.listener, LISTENER, Interest::READABLE)?;
        let mut torrents = self.torrents.drain().collect();
        for (_, torrent) in &mut torrents {
            self.add_peers(torrent);
        }
        self.torrents = torrents;
        Ok(())
    }

    pub fn poll(&mut self) -> Result<(), Box<dyn Error>> {
        self.maybe_print_info();
        self.poll
            .poll(&mut self.events, Some(std::time::Duration::from_secs(1)))?;
        if self.events.is_empty() {
            info!("Poll returned no events");
        }
        // let events = take(self.events);
        for event in &self.events {
            match event.token() {
                LISTENER => loop {
                    match self.listener.accept() {
                        Ok((stream, _)) => {
                            let mut peer = HandshakingConnection::new_from_incoming(
                                stream,
                                self.next_socket_index,
                            );
                            let token = Token(self.next_socket_index);
                            self.next_socket_index += 1;
                            peer.register(&mut self.poll, token, Interest::READABLE);
                            self.connections
                                .insert(token, Connection::Handshaking(peer));
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            break;
                        }
                        e => info!("Failed to accept peer: {:?}", e),
                    }
                },
                token => {
                    let peer = self.connections.get_mut(&token).unwrap();
                    let handshake = match peer {
                        Connection::Handshaking(connection) => {
                            if event.is_writable() {
                                // Writable means it is now connected
                                connection.reregister(&mut self.poll, token, Interest::READABLE);
                            }
                            match connection.update() {
                                Ok(status) => match status {
                                    HandshakeUpdateSuccess::NoUpdate => {
                                        continue;
                                    }
                                    HandshakeUpdateSuccess::Complete(handshake) => handshake,
                                },
                                Err(error) => {
                                    info!("Removing peer {}: {} while updating", token.0, error);
                                    connection.deregister(&mut self.poll);
                                    self.connections.remove(&token);
                                    continue;
                                }
                            }
                        }
                        Connection::Established(connection) => match connection.update() {
                            Ok(status) => match status {
                                UpdateSuccess::Transferred {
                                    downloaded,
                                    uploaded,
                                } => {
                                    self.downloaded += downloaded;
                                    self.uploaded += uploaded;
                                    continue;
                                }
                                UpdateSuccess::NoUpdate | UpdateSuccess::Success => {
                                    continue;
                                }
                            },
                            Err(error) => {
                                info!("Removing peer {}: {} while updating", token.0, error);
                                connection.deregister(&mut self.poll);
                                self.connections.remove(&token);
                                continue;
                            }
                        },
                    };
                    let torrent_data = self.torrents.get(&handshake.info_hash).unwrap(); // TODO make this unwrap safe by confirming we have the info_hash during the handshake
                    let peer = self.connections.remove_entry(&token).unwrap().1;
                    if let Connection::Handshaking(connection) = peer {
                        let mut promoted = EstablishedConnection::new_from_handshaking(
                            connection,
                            torrent_data.piece_info.total_pieces,
                            torrent_data.piece_assigner.clone(),
                            torrent_data.piece_store.clone(),
                        );
                        // Same as above, factor out
                        match promoted.update() {
                            Ok(status) => match status {
                                UpdateSuccess::Transferred {
                                    downloaded,
                                    uploaded,
                                } => {
                                    self.downloaded += downloaded;
                                    self.uploaded += uploaded;
                                }
                                UpdateSuccess::NoUpdate | UpdateSuccess::Success => {}
                            },
                            Err(error) => {
                                info!("Removing peer {}: {} while updating", token.0, error);
                                promoted.deregister(&mut self.poll);
                                self.connections.remove(&token);
                            }
                        }
                        // fix these, as failing them should be handled like the rest of the errors here TODO
                        info!("Bitfield: {:?}", torrent_data.piece_store.borrow().have());
                        promoted
                            .send(&Bitfield {
                                bitfield: torrent_data.piece_store.borrow().have(),
                            })
                            .unwrap();
                        promoted.send(&Unchoke {}).unwrap();
                        promoted.send(&Interested {}).unwrap();
                        self.connections
                            .insert(token, Connection::Established(promoted));
                    }
                }
            }
        }
        self.send_haves();
        Ok(())
    }

    pub fn run(&mut self) -> Result<(), Box<dyn Error>> {
        self.start()?;
        // TODO:
        // If it has been reponse.interval time, re-announce to tracker
        // Don't allow incoming connections past 50 peers
        // Implement responding to block requests
        // Implement both sending and receiving of cancel messages
        loop {
            self.poll()?;
            // TODO: Can the download stall?  If no reads are happening, we won't ever request more.
            // E.g., the following series of events:
            // - Endgame mode starts
            // - Initial batch of unreceived runs out, but we throttle calls to that function
            // - Peer 1 grabbed a request from that initial batch, but failed a hash check
            // - Peer 2 got throttled when calling unreceived(), so they didn't request any
            // - Peer 2 will possibly never start downloading pieces now, since it didn't request any
            //
            // We can fix this by simply looping over the connections and requesting they request
            // more pieces if they are under their limit.  We could do this if the Poll times out,
            // but then we could have a peer sending bad data that prevents that logic from being run.
            // So we need to do this only if we haven't made forward progress in a given amount of time.
            // if self.connections.len() < self.config.max_peers {
            //     for (_, torrent) in &self.torrents {
            //         self.add_peers(torrent);
            //     }
            // }
            // if self.piece_store.borrow().done() {
            //     assert!(self.piece_store.borrow().verify_all_files());
            //     break;
            // }
            if self.stop {
                break;
            }
        }
        // disconnect from seeders
        // loop {
        //     self.maybe_print_info();
        //     self.poll
        //         .poll(&mut events, Some(std::time::Duration::from_secs(1)))?;
        //     for event in &events {
        //         match event.token() {
        //             LISTENER => self.accept_connections(&mut listener),
        //             token => {
        //                 let _ = self.handle_event(token, Some(event));
        //             }
        //         }
        //     }
        //     // if self.connections.len() < self.config.max_peers {
        //     //     for (_, torrent) in &self.torrents {
        //     //         self.add_peers(torrent);
        //     //     }
        //     // }
        // }
        Ok(())
    }

    // Accept incoming connections
    // fn accept_connections(&mut self, listener: &mut TcpListener) {
    //     loop {
    //         match listener.accept() {
    //             Ok((stream, _)) => {
    //                 let mut peer =
    //                     HandshakingConnection::new_from_incoming(stream, self.next_socket_index);
    //                 let token = Token(self.next_socket_index);
    //                 self.next_socket_index += 1;
    //                 peer.register(&mut self.poll, token, Interest::READABLE);
    //                 self.connections
    //                     .insert(token, Connection::Handshaking(peer));
    //             }
    //             Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
    //                 break;
    //             }
    //             e => panic!("err={:?}", e),
    //         }
    //     }
    // }

    fn stop(&mut self) {
        self.stop = true;
    }

    // Connect to more peers if needed
    fn add_peers(&mut self, torrent: &mut TorrentData<T>) {
        let free = self.config.max_peers - self.connections.len();
        if free <= 0 || torrent.peer_list.len() == 0 {
            return;
        }
        let available = torrent.peer_list.len();
        let drain_amount = std::cmp::min(available, free);
        for peer_info in torrent.peer_list.drain(..drain_amount) {
            let stream = if let Ok(stream) = TcpStream::connect(peer_info.addr) {
                stream
            } else {
                info!("Unable to connect to {}", peer_info.addr);
                continue;
            };
            stream.set_nodelay(true).unwrap();
            let mut peer = HandshakingConnection::new_from_outgoing(
                stream,
                torrent.torrent.metainfo.info_hash_raw,
                self.next_socket_index,
            );
            let token = Token(self.next_socket_index);
            self.next_socket_index += 1;
            // Registering the socket for Writable notifications will tell us when it is connected.
            peer.register(&mut self.poll, token, Interest::WRITABLE);
            self.connections
                .insert(token, Connection::Handshaking(peer));
        }
    }

    // Currently no validation of peers.  We need to prevent us from connecting to the same peer twice.
    // fn get_peers(
    //     &self,
    //     peer_list: &mut Vec<PeerInfo>,
    //     info_hash: Sha1Hash,
    // ) -> Vec<HandshakingConnection> {
    //     let mut connections = Vec::new();
    //     let mut socket_index = self.next_socket_index;
    //     let free = self.config.max_peers - self.connections.len();
    //     if free <= 0 || peer_list.len() == 0 {
    //         return connections;
    //     }
    //     let available = peer_list.len();
    //     let drain_amount = std::cmp::min(available, free);
    //     for peer_info in peer_list.drain(..drain_amount) {
    //         let stream = if let Ok(stream) = TcpStream::connect(peer_info.addr) {
    //             stream
    //         } else {
    //             info!("Unable to connect to {}", peer_info.addr);
    //             continue;
    //         };
    //         let mut peer =
    //             HandshakingConnection::new_from_outgoing(stream, info_hash, socket_index);
    //         let token = Token(socket_index);
    //         socket_index += 1;
    //         // Registering the socket for Writable notifications will tell us when it is connected.
    //         // peer.register(&mut self.poll, token, Interest::WRITABLE);
    //         // self.connections
    //         //     .insert(token, Connection::Handshaking(peer));
    //         connections.push(peer);
    //     }
    //     connections
    // }

    // fn handle_event_impl<T: ConnectionBase>(
    //     &mut self,
    //     peer: &mut T,
    //     token: Token,
    //     event: Option<&Event>,
    // ) -> T::UpdateSuccessType {
    //     if let Some(e) = event {
    //         if e.is_writable() {
    //             // Writable means it is now connected
    //             peer.reregister(&mut self.poll, token, Interest::READABLE);
    //         }
    //     }
    //     match peer.update() {
    //         Ok(status) => status,
    //         Err(error) => {
    //             info!("Removing peer {}: {} while updating", token.0, error);
    //             peer.deregister(&mut self.poll);
    //             self.connections.remove(&token);
    //             T::UpdateSuccessType::default()
    //         }
    //     }
    // }

    // Handle Poll event
    // fn handle_event(&mut self, token: Token, event: Option<&Event>) {
    //     let peer = self.connections.get_mut(&token).unwrap();
    //     let handshake = match peer {
    //         Connection::Handshaking(connection) => {
    //             if let Some(e) = event {
    //                 if e.is_writable() {
    //                     // Writable means it is now connected
    //                     connection.reregister(&mut self.poll, token, Interest::READABLE);
    //                 }
    //             }
    //             match connection.update() {
    //                 Ok(status) => match status {
    //                     HandshakeUpdateSuccess::NoUpdate => {
    //                         return;
    //                     }
    //                     HandshakeUpdateSuccess::Complete(handshake) => handshake,
    //                 },
    //                 Err(error) => {
    //                     info!("Removing peer {}: {} while updating", token.0, error);
    //                     connection.deregister(&mut self.poll);
    //                     self.connections.remove(&token);
    //                     return;
    //                 }
    //             }
    //         }
    //         Connection::Established(connection) => match connection.update() {
    //             Ok(status) => match status {
    //                 UpdateSuccess::Transferred {
    //                     downloaded,
    //                     uploaded,
    //                 } => {
    //                     self.downloaded += downloaded;
    //                     self.uploaded += uploaded;
    //                     return;
    //                 }
    //                 UpdateSuccess::NoUpdate | UpdateSuccess::Success => {
    //                     return;
    //                 }
    //             },
    //             Err(error) => {
    //                 info!("Removing peer {}: {} while updating", token.0, error);
    //                 connection.deregister(&mut self.poll);
    //                 self.connections.remove(&token);
    //                 return;
    //             }
    //         },
    //     };
    //     let torrent_data = self.torrents.get(&handshake.info_hash).unwrap(); // TODO make this unwrap safe by confirming we have the info_hash during the handshake
    //     let peer = self.connections.remove_entry(&token).unwrap().1;
    //     if let Connection::Handshaking(connection) = peer {
    //         let promoted = EstablishedConnection::new_from_handshaking(
    //             connection,
    //             torrent_data.piece_info.total_pieces,
    //             torrent_data.piece_assigner.clone(),
    //             torrent_data.piece_store.clone(),
    //         );
    //         self.connections
    //             .insert(token, Connection::Established(promoted));
    //     }
    // }

    fn maybe_print_info(&mut self) {
        if !self.config.print_output {
            return;
        }
        for (_, torrent) in &self.torrents {
            let now = std::time::Instant::now();
            if now - self.last_update > PRINT_UPDATE_TIME {
                let download_rate = (self.downloaded as f64) / ((1 << 20) as f64);
                let upload_rate = (self.uploaded as f64) / ((1 << 20) as f64);
                self.downloaded = 0;
                self.uploaded = 0;
                // print!("{esc}[2J{esc}[1;1H", esc = 27 as char);
                let have = torrent.piece_store.borrow().have();
                let ticks = 200;
                let pieces_per_tick = have.len() / ticks;
                let mut progress_bar = String::with_capacity(ticks);
                for tick in 0..ticks {
                    let start_index = tick * pieces_per_tick;
                    let section: BitVec<u32> =
                        BitVec::from_iter(have.iter().skip(start_index).take(pieces_per_tick));
                    if section.all() {
                        progress_bar.push('X');
                    } else {
                        progress_bar.push('_');
                    }
                }
                print!("{}\n", progress_bar);
                print!(
                    "Percent done: {:.2}% Download: {:.2} MiB/s Upload: {:.2} MiB/s Peers: {}\n",
                    torrent.piece_store.borrow().percent_done(),
                    download_rate,
                    upload_rate,
                    self.connections.len(),
                );
                info!("Download: {:.2} MiB/s", download_rate);
                let mut temp = Vec::new();
                for (k, v) in self.connections.iter() {
                    temp.push((k, v));
                }
                // temp.sort_by(|a, b| b.1.downloaded.cmp(&a.1.downloaded));
                // for (id, conn) in temp.iter().take(5) {
                //     print!("Connection {}: {} bytes\n", id.0, conn.downloaded);
                // }
                use std::io::prelude::*;
                std::io::stdout()
                    .flush()
                    .ok()
                    .expect("Could not flush stdout");
                self.last_update = now;
            }
        }
    }

    // Send have messages to peers that do not have the associated piece.
    // Todo:
    // take into account if peer is choking?
    fn send_haves(&mut self) {
        while let Ok(have) = self.recv_have.try_recv() {
            let mut to_remove = Vec::new();
            for (id, connection) in &mut self.connections {
                if let Connection::Established(peer) = connection {
                    if peer.peer_has[have.index] {
                        continue;
                    }
                    if let Err(error) = peer.send(&have) {
                        info!("Disconnecting peer {}: {} while sending", id.0, error);
                        peer.deregister(&mut self.poll);
                        to_remove.push(*id);
                    }
                }
            }
            for id in to_remove {
                self.connections.remove(&id);
            }
        }
    }
}
