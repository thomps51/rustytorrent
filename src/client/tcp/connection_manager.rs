use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    time::{Duration, Instant},
};

use log::{debug, info};
use mio::{
    net::{TcpListener, TcpStream},
    Interest, Poll, Token,
};
use rccell::RcCell;

use crate::{
    client::{
        connection_manager::TorrentData,
        disk_manager::ConnectionIdentifier,
        tcp::{established::EstablishedConnection, handshaking::HandshakingConnection},
        UpdateError, UpdateSuccess,
    },
    common::{Sha1Hash, PEER_ID_LENGTH},
    io::ReadBuffer,
    messages::{Bitfield, Block, Handshake, Interested, Unchoke},
    tracker::PeerInfo,
};

use super::{handshaking::HandshakeUpdateSuccess, Connection, ConnectionBase};

pub struct TcpConnectionManager {
    peer_id: [u8; PEER_ID_LENGTH],
    next_socket_id: usize,
    read_buffer: ReadBuffer,
    listener: TcpListener,
    connections: Vec<Connection>,
    poll: RcCell<Poll>,
    available_socket_indexes: VecDeque<(usize, Instant)>,
}

impl TcpConnectionManager {
    pub fn new(peer_id: [u8; PEER_ID_LENGTH], listener: TcpListener, poll: RcCell<Poll>) -> Self {
        Self {
            connections: Vec::new(),
            listener,
            peer_id,
            read_buffer: ReadBuffer::new(1 << 20), // 1 MiB
            poll,
            available_socket_indexes: VecDeque::new(),
            next_socket_id: 0,
        }
    }

    fn add_connection(&mut self, connection: Connection, index: usize) {
        match index.cmp(&self.connections.len()) {
            std::cmp::Ordering::Less => self.connections[index] = connection,
            std::cmp::Ordering::Equal => self.connections.push(connection),
            std::cmp::Ordering::Greater => panic!("Make this an error"),
        }
    }

    fn get_next_connection_index(&self) -> usize {
        if !self.available_socket_indexes.is_empty() {
            let (index, last_used) = *self.available_socket_indexes.front().unwrap();
            if Instant::now() - last_used < (Duration::SECOND * 60 * 5) {
                return index;
            }
        }
        self.connections.len()
    }

    // Accept incoming connections
    pub fn accept_connections(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    let token = Token(self.get_next_connection_index());
                    let id = self.next_socket_id;
                    self.next_socket_id += 1;
                    self.poll
                        .borrow()
                        .registry()
                        .register(&mut stream, token, Interest::READABLE | Interest::WRITABLE)
                        .unwrap();
                    let peer = HandshakingConnection::new_from_incoming(
                        Box::new(stream),
                        ConnectionIdentifier::TcpToken(token, id),
                        self.peer_id,
                    );
                    self.add_connection(Connection::Handshaking(peer), token.0);
                    debug!("Accepted new connection");
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    break;
                }
                e => panic!("err={e:?}"), // TODO: when does this fail?
            }
        }
    }

    pub fn handle_event(&mut self, torrents: &mut HashMap<Sha1Hash, TorrentData>, token: Token) {
        let peer = &mut self.connections[token.0];
        let mut info_hash = None;
        let result = try {
            match peer {
                Connection::Handshaking(connection) => {
                    match connection.update(&mut self.read_buffer)? {
                        HandshakeUpdateSuccess::NoUpdate => {}
                        HandshakeUpdateSuccess::Complete(handshake) => {
                            info_hash = Some(handshake.info_hash);
                            self.promote(torrents, token, handshake)?
                        }
                    }
                }
                Connection::Established(connection) => {
                    let result = connection.update(&mut self.read_buffer);
                    match result {
                        Ok(UpdateSuccess::Transferred {
                            downloaded: _,
                            uploaded: _,
                        }) => {
                            // self.downloaded += downloaded;
                            // self.uploaded += uploaded;
                        }
                        Ok(UpdateSuccess::NoUpdate | UpdateSuccess::Success) => {}
                        Err(error) => {
                            info_hash = Some(connection.info_hash());
                            Result::Err(error)?;
                        }
                    };
                }
                Connection::Empty => panic!("this should be unregistered"),
            };
        };
        if let Err(error) = result
            && let Some(info_hash) = info_hash
            && let Some(torrent) = torrents.get_mut(&info_hash)
        {
            // If it was a TCP connnection that didn't finish handshaking, try again on UDP?
            self.disconnect_peer(torrent, token, error);
        }
    }

    fn promote(
        &mut self,
        torrents: &mut HashMap<Sha1Hash, TorrentData>,
        token: Token,
        handshake: Handshake,
    ) -> Result<(), UpdateError> {
        debug!("Handshake received, promoting to established connection");
        let torrent_data = if let Some(torrent_data) = torrents.get_mut(&handshake.info_hash) {
            torrent_data
        } else {
            return Err(UpdateError::TorrentNotManaged {
                info_hash: handshake.info_hash,
            });
        };
        if torrent_data.peers_data.is_connected(&handshake.peer_id)
            || handshake.peer_id == self.peer_id
        {
            return Err(UpdateError::CommunicationError(
                std::io::ErrorKind::AlreadyExists.into(),
            ));
        }
        let peer = self.connections[token.0].take();
        if let Connection::Handshaking(connection) = peer {
            let (network_source, id) = connection.into_source_id();
            torrent_data.peers_data.add_connected_id(
                token,
                handshake.peer_id,
                network_source.peer_addr()?,
            );
            let mut promoted = EstablishedConnection::new(
                id,
                torrent_data.info_hash,
                network_source,
                torrent_data.piece_info.total_pieces,
                torrent_data.piece_assigner.clone(),
                torrent_data.disk_requester.clone(),
                torrent_data.block_cache.clone(),
                torrent_data.downloaded.clone(),
                torrent_data.uploaded.clone(),
            );
            promoted.send(&Bitfield {
                bitfield: torrent_data.have_on_disk.clone(),
            })?;
            promoted.send(&Unchoke {})?;
            promoted.send(&Interested {})?;
            self.connections[token.0] = Connection::Established(promoted);
        }
        // Need to call update logic on promoted connection in case we missed reading any messages
        self.handle_event(torrents, token);
        Ok(())
    }

    pub fn disconnect_peer(
        &mut self,
        torrent: &mut TorrentData,
        token: Token,
        error: UpdateError,
    ) -> Option<SocketAddr> {
        info!("Removing peer {}: {} while updating", token.0, error);
        torrent.peers_data.disconnected(token);
        let connection = self.connections[token.0].take();
        let mut source = connection.into_network_source();
        self.poll
            .borrow()
            .registry()
            .deregister(&mut source)
            .unwrap();
        source.peer_addr().ok()
    }

    pub fn connect_to(&mut self, torrent: &mut TorrentData, peer_info: PeerInfo) {
        let mut stream = match TcpStream::connect(peer_info.addr) {
            Ok(stream) => stream,
            Err(error) => {
                info!("Unable to connect to {}: {:?}", peer_info.addr, error);
                return;
            }
        };
        // let token = Token(self.next_socket_index);
        let token = Token(self.get_next_connection_index());
        let id = self.next_socket_id;
        torrent.peers_data.connected(token, peer_info);
        self.next_socket_id += 1;
        // Registering the socket for Writable notifications will tell us when it is connected.
        self.poll
            .borrow()
            .registry()
            .register(&mut stream, token, Interest::READABLE | Interest::WRITABLE)
            .unwrap();
        // If this fails, retry with UDP?
        let peer = HandshakingConnection::new_from_outgoing(
            Box::new(stream),
            torrent.info_hash,
            ConnectionIdentifier::TcpToken(token, id),
            self.peer_id,
        );
        self.add_connection(Connection::Handshaking(peer), token.0);
    }

    pub fn send_block(&mut self, torrent: &mut TorrentData, block: Block, token: Token, id: usize) {
        let Connection::Established(connection) = &mut self.connections[token.0] else {return};
        if connection.id() != ConnectionIdentifier::TcpToken(token, id) {
            // We must have disconnected and this is a new connection at the same token index.
            return;
        }
        // In theory we could have disconnected before we send this
        let Err(error) = connection.send_block(&block) else {return};
        self.disconnect_peer(torrent, token, error.into());
    }

    pub fn num_connections(&self) -> usize {
        self.connections.len()
    }

    // Send have messages to peers that do not have the associated piece.
    pub fn send_have(
        &mut self,
        torrents: &mut HashMap<Sha1Hash, TorrentData>,
        info_hash: Sha1Hash,
        piece_index: usize,
    ) {
        if let Some(torrent) = torrents.get_mut(&info_hash) {
            torrent.have_on_disk.set(piece_index, true);
            for token in torrent.peers_data.token_to_info.keys() {
                if let Connection::Established(peer) = &mut self.connections[token.0] {
                    peer.send_have(piece_index);
                }
            }
        }
    }
}
