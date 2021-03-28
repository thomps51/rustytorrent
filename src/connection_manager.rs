use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error;
use std::rc::Rc;
use std::sync::mpsc;

use log::info;
use mio::event::Event;
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token};

use crate::connection::Connection;
use crate::connection::*;
use crate::messages::Have;
use crate::piece_assigner::PieceAssigner;
use crate::piece_store::{FileSystem, PieceStore};
use crate::torrent::Torrent;
use crate::tracker::EventKind;
use crate::tracker::PeerInfo;
use crate::tracker::Tracker;
use crate::SharedPieceAssigner;
use crate::SharedPieceStore;

const LISTENER: Token = Token(std::usize::MAX);
const LISTEN_PORT: u16 = 6881;
const MAX_PEERS: usize = 60;
const PRINT_UPDATE_TIME: std::time::Duration = std::time::Duration::from_secs(1);

pub struct ConnectionManager {
    connections: HashMap<Token, Connection>,
    downloaded: usize,
    uploaded: usize,
    last_update: std::time::Instant,
    next_socket_index: usize,
    peer_list: Vec<PeerInfo>,
    piece_assigner: SharedPieceAssigner,
    piece_store: SharedPieceStore,
    poll: Poll,
    recv_have: mpsc::Receiver<Have>,
    torrent: Torrent,
    tracker: Tracker,
}

impl ConnectionManager {
    pub fn new(torrent: Torrent) -> ConnectionManager {
        let tracker = Tracker {
            address: torrent.metainfo.announce.clone(),
            listen_port: LISTEN_PORT,
        };
        let num_pieces = torrent.metainfo.pieces.len();
        let piece_length = torrent.metainfo.piece_length;
        let (send, recv) = mpsc::channel();
        let (send_have, recv_have) = mpsc::channel();
        info!("Piece length: {}, Num pieces: {}", piece_length, num_pieces);
        let piece_assigner = Rc::new(RefCell::new(PieceAssigner::new(
            num_pieces,
            torrent.metainfo.total_size,
            piece_length,
            recv,
        )));
        let piece_store: Rc<RefCell<_>> = Rc::new(RefCell::new(
            FileSystem::new(&torrent, send, send_have).unwrap(),
        ));
        let poll = Poll::new().unwrap();
        ConnectionManager {
            connections: HashMap::new(),
            downloaded: 0,
            uploaded: 0,
            last_update: std::time::Instant::now(),
            next_socket_index: 0,
            peer_list: Vec::new(),
            piece_assigner,
            piece_store,
            poll,
            recv_have,
            torrent,
            tracker,
        }
    }

    pub fn run(&mut self) -> Result<(), Box<dyn Error>> {
        let response = self.tracker.announce(&self.torrent, EventKind::Started)?;
        self.peer_list = response.peer_list;
        let mut events = Events::with_capacity(1024);
        let mut listener = TcpListener::bind(format!("0.0.0.0:{}", LISTEN_PORT).as_str().parse()?)?;
        self.poll
            .registry()
            .register(&mut listener, LISTENER, Interest::READABLE)?;
        self.add_peers();
        // TODO:
        // If it has been reponse.interval time, re-announce to tracker
        // Don't allow incoming connections past 50 peers
        // Implement responding to block requests
        // Implement both sending and receiving of cancel messages
        loop {
            self.print_info();
            self.poll
                .poll(&mut events, Some(std::time::Duration::from_secs(1)))?;
            for event in &events {
                match event.token() {
                    LISTENER => self.accept_connections(&mut listener),
                    token => self.handle_event(token, Some(event)),
                }
            }
            self.send_haves();
            if self.connections.len() < MAX_PEERS {
                self.add_peers();
            }
            if self.piece_store.borrow().done() {
                assert!(self.piece_store.borrow().verify_all_files());
                break;
            }
        }
        Ok(())
    }

    // Accept incoming connections
    fn accept_connections(&mut self, listener: &mut TcpListener) {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let num_pieces = self.torrent.metainfo.pieces.len();
                    let mut peer = Connection::new_from_incoming(
                        num_pieces,
                        stream,
                        self.piece_assigner.clone(),
                        self.piece_store.clone(),
                        self.torrent.metainfo.info_hash_raw,
                        self.next_socket_index,
                    );
                    let token = Token(self.next_socket_index);
                    self.next_socket_index += 1;
                    peer.register(&mut self.poll, token, Interest::READABLE);
                    self.connections.insert(token, peer);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    break;
                }
                e => panic!("err={:?}", e),
            }
        }
    }

    // Connect to more peers if needed
    fn add_peers(&mut self) {
        let free = MAX_PEERS - self.connections.len();
        if free <= 0 || self.peer_list.len() == 0 {
            return;
        }
        let available = self.peer_list.len();
        let drain_amount = std::cmp::min(available, free);
        for peer_info in self.peer_list.drain(..drain_amount) {
            let num_pieces = self.torrent.metainfo.pieces.len();
            let stream = if let Ok(stream) = TcpStream::connect(peer_info.addr) {
                stream
            } else {
                info!("Unable to connect to {}", peer_info.addr);
                continue;
            };
            let mut peer = Connection::new_from_outgoing(
                peer_info.addr,
                num_pieces,
                stream,
                self.piece_assigner.clone(),
                self.piece_store.clone(),
                self.torrent.metainfo.info_hash_raw,
                self.next_socket_index,
            );
            let token = Token(self.next_socket_index);
            self.next_socket_index += 1;
            // Registering the socket for Writable notifications will tell us when it is connected.
            peer.register(&mut self.poll, token, Interest::WRITABLE);
            self.connections.insert(token, peer);
        }
    }

    // Handle Poll event
    fn handle_event(&mut self, token: Token, event: Option<&Event>) {
        let peer = self.connections.get_mut(&token).unwrap();
        if let Some(e) = event {
            if e.is_writable() {
                // Writable means it is now connected
                peer.reregister(&mut self.poll, token, Interest::READABLE);
            }
        }
        loop {
            match peer.update() {
                Ok(status) => {
                    if let UpdateSuccess::Transferred {
                        downloaded,
                        uploaded,
                    } = status
                    {
                        self.downloaded += downloaded;
                        self.uploaded += uploaded;
                    }
                    if let UpdateSuccess::NoUpdate = status {
                        break;
                    }
                }
                Err(error) => {
                    info!("Removing peer {}: {} while updating", token.0, error);
                    peer.deregister(&mut self.poll);
                    self.connections.remove(&token);
                    break;
                }
            }
        }
    }

    fn print_info(&mut self) {
        let now = std::time::Instant::now();
        if now - self.last_update > PRINT_UPDATE_TIME {
            let download_rate = (self.downloaded as f64) / ((1 << 20) as f64);
            self.downloaded = 0;
            self.uploaded = 0;
            print!("{esc}[2J{esc}[1;1H", esc = 27 as char);
            print!(
                "Percent done: {:.2}% Download: {:.2} MiB/s Peers: {}\n",
                self.piece_store.borrow().percent_done(),
                download_rate,
                self.connections.len(),
            );
            info!("Download: {:.2} MiB/s", download_rate);
            let mut temp = Vec::new();
            for (k, v) in self.connections.iter() {
                temp.push((k, v));
            }
            temp.sort_by(|a, b| b.1.downloaded.cmp(&a.1.downloaded));
            for (id, conn) in temp.iter().take(5) {
                print!("Connection {}: {} bytes\n", id.0, conn.downloaded);
            }

            use std::io::prelude::*;
            std::io::stdout()
                .flush()
                .ok()
                .expect("Could not flush stdout");
            self.last_update = now;
        }
    }

    // Send have messages to peers that do not have the associated piece.
    // Todo:
    // take into account if peer is choking?
    fn send_haves(&mut self) {
        while let Ok(have) = self.recv_have.try_recv() {
            let mut to_remove = Vec::new();
            for (id, peer) in &mut self.connections {
                if peer.peer_has[have.index] {
                    continue;
                }
                if let Err(error) = peer.send(&have) {
                    info!("Disconnecting peer {}: {} while sending", id.0, error);
                    peer.deregister(&mut self.poll);
                    to_remove.push(*id);
                }
            }
            for id in to_remove {
                self.connections.remove(&id);
            }
        }
    }
}
