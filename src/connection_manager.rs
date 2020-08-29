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
use crate::messages::{Have, Message};
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
const MAX_PEERS: usize = 30;
const PRINT_UPDATE_TIME: std::time::Duration = std::time::Duration::from_secs(1);

pub struct ConnectionManager {
    connections: HashMap<Token, Connection>,
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
        let mut listener =
            TcpListener::bind(format!("10.0.0.2:{}", LISTEN_PORT).as_str().parse()?)?;
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
                    token => self.handle_event(token, event),
                }
            }
            self.send_haves();
            if self.connections.len() < MAX_PEERS {
                self.add_peers();
            }
            if self.piece_store.borrow().done() {
                break;
            }
        }
        Ok(())
    }

    fn accept_connections(&mut self, listener: &mut TcpListener) {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    println!("new peer!");
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
                    self.poll
                        .registry()
                        .register(&mut peer.stream, token, Interest::READABLE)
                        .unwrap();
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
            self.poll
                .registry()
                .register(&mut peer.stream, token, Interest::WRITABLE)
                .unwrap();
            self.connections.insert(token, peer);
        }
    }

    fn handle_event(&mut self, token: Token, event: &Event) {
        loop {
            let peer = self.connections.get_mut(&token).unwrap();
            if event.is_writable() {
                // Writable means it is now connected
                self.poll
                    .registry()
                    .reregister(&mut peer.stream, token, Interest::READABLE)
                    .unwrap();
            }
            match peer.update() {
                Ok(status) => {
                    if let UpdateSuccess::NoUpdate = status {
                        break;
                    }
                }
                Err(error) => {
                    info!("Removing peer {}: {}", token.0, error);
                    self.poll.registry().deregister(&mut peer.stream).unwrap();
                    self.connections.remove(&token);
                    break;
                }
            }
            if self.piece_store.borrow().done() {
                break;
            }
        }
    }

    fn print_info(&mut self) {
        let now = std::time::Instant::now();
        if now - self.last_update > PRINT_UPDATE_TIME {
            print!(
                "Percent done: {:.2}%\r",
                self.piece_store.borrow().percent_done()
            );
            use std::io::prelude::*;
            std::io::stdout()
                .flush()
                .ok()
                .expect("Could not flush stdout");
            self.last_update = now;
        }
    }

    // Send have messages to peers that do not have the associated piece.
    fn send_haves(&mut self) {
        while let Ok(have) = self.recv_have.try_recv() {
            let mut to_remove = Vec::new();
            for (id, conn) in &mut self.connections {
                if conn.peer_has[have.index] {
                    continue;
                }
                if let Err(error) = have.write_to(&mut conn.stream) {
                    info!("Disconnecting peer {}: {}", id.0, error);
                }
                self.poll.registry().deregister(&mut conn.stream).unwrap();
                to_remove.push(*id);
            }
            for id in to_remove {
                self.connections.remove(&id);
            }
        }
    }
}
