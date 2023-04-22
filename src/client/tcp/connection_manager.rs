use std::{collections::HashMap, net::SocketAddr};

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
    messages::{Bitfield, Block, Handshake, Have, Interested, Unchoke},
    tracker::PeerInfo,
};

use super::{handshaking::HandshakeUpdateSuccess, Connection, ConnectionBase};

pub struct TcpConnectionManager {
    peer_id: [u8; PEER_ID_LENGTH],
    next_socket_index: usize,
    read_buffer: ReadBuffer,
    token_to_info_hash: HashMap<Token, Sha1Hash>,
    listener: TcpListener,
    connections: HashMap<Token, Connection>,
    poll: RcCell<Poll>,
}

impl TcpConnectionManager {
    pub fn new(peer_id: [u8; PEER_ID_LENGTH], listener: TcpListener, poll: RcCell<Poll>) -> Self {
        Self {
            connections: HashMap::new(),
            next_socket_index: 0,
            listener,
            peer_id,
            token_to_info_hash: HashMap::new(),
            read_buffer: ReadBuffer::new(1 << 20), // 1 MiB
            poll,
        }
    }

    // Accept incoming connections
    pub fn accept_connections(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    let token = Token(self.next_socket_index);
                    self.next_socket_index += 1;
                    self.poll
                        .borrow()
                        .registry()
                        .register(&mut stream, token, Interest::READABLE | Interest::WRITABLE)
                        .unwrap();
                    let peer = HandshakingConnection::new_from_incoming(
                        Box::new(stream),
                        token,
                        self.peer_id,
                    );
                    debug!("Accepted new connection");
                    self.connections
                        .insert(token, Connection::Handshaking(peer));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    break;
                }
                e => panic!("err={e:?}"), // TODO: when does this fail?
            }
        }
    }

    pub fn handle_event(&mut self, torrents: &mut HashMap<Sha1Hash, TorrentData>, token: Token) {
        if let Err(error) = self.handle_event_inner(torrents, token) {
            // If it was a TCP connnection that didn't finish handshaking, try again on UDP?
            self.disconnect_peer(torrents, token, error);
        }
    }

    pub fn handle_event_inner(
        &mut self,
        torrents: &mut HashMap<Sha1Hash, TorrentData>,
        token: Token,
    ) -> Result<(), UpdateError> {
        let Some(peer) = self.connections.get_mut(&token) else {
            return Ok(());
        };
        match peer {
            Connection::Handshaking(connection) => {
                match connection.update(&mut self.read_buffer)? {
                    HandshakeUpdateSuccess::NoUpdate => {}
                    HandshakeUpdateSuccess::Complete(handshake) => {
                        self.promote(torrents, token, handshake)?
                    }
                }
            }
            Connection::Established(connection) => {
                match connection.update(&mut self.read_buffer)? {
                    UpdateSuccess::Transferred {
                        downloaded: _,
                        uploaded: _,
                    } => {
                        // self.downloaded += downloaded;
                        // self.uploaded += uploaded;
                    }
                    UpdateSuccess::NoUpdate | UpdateSuccess::Success => {}
                }
            }
        };
        Ok(())
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
        let peer = self.connections.remove_entry(&token).unwrap().1;
        if let Connection::Handshaking(connection) = peer {
            self.token_to_info_hash.insert(token, handshake.info_hash);
            let network_source = connection.into_network_source();
            torrent_data.peers_data.add_connected_id(
                token,
                handshake.peer_id,
                network_source.peer_addr()?,
            );
            let mut promoted = EstablishedConnection::new(
                ConnectionIdentifier::TcpToken(token),
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
            self.connections
                .insert(token, Connection::Established(promoted));
        }
        // Need to call update logic on promoted connection in case we missed reading any messages
        self.handle_event_inner(torrents, token)
    }

    pub fn disconnect_peer(
        &mut self,
        torrents: &mut HashMap<Sha1Hash, TorrentData>,
        token: Token,
        error: UpdateError,
    ) -> Option<SocketAddr> {
        info!("Removing peer {}: {} while updating", token.0, error);
        if let Some(info_hash) = self.token_to_info_hash.remove(&token) {
            if let Some(torrent_data) = torrents.get_mut(&info_hash) {
                torrent_data.peers_data.disconnected(token);
            }
        }
        if let Some(connection) = self.connections.remove(&token) {
            let mut source = connection.into_network_source();
            self.poll
                .borrow()
                .registry()
                .deregister(&mut source)
                .unwrap();
            return source.peer_addr().ok();
        }
        None
    }

    pub fn connect_to(&mut self, torrent: &mut TorrentData, peer_info: PeerInfo) {
        let mut stream = match TcpStream::connect(peer_info.addr) {
            Ok(stream) => stream,
            Err(error) => {
                info!("Unable to connect to {}: {:?}", peer_info.addr, error);
                return;
            }
        };
        let token = Token(self.next_socket_index);
        torrent.peers_data.connected(token, peer_info);
        self.next_socket_index += 1;
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
            token,
            self.peer_id,
        );
        self.connections
            .insert(token, Connection::Handshaking(peer));
    }

    pub fn send_block(
        &mut self,
        torrents: &mut HashMap<Sha1Hash, TorrentData>,
        block: Block,
        token: Token,
    ) {
        let Some(Connection::Established(connection)) =
            self.connections.get_mut(&token) else {return};
        let Err(error) = connection.send(&block) else {return};
        self.disconnect_peer(torrents, token, error.into());
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
                if let Some(Connection::Established(peer)) = self.connections.get_mut(token) {
                    if peer.peer_data.peer_has[piece_index] {
                        continue;
                    }
                    peer.write_to_send_buffer(Have {
                        index: piece_index as u32,
                    });
                }
            }
        }
    }
}
