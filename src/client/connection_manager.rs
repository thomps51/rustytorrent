use rand::{distributions::Alphanumeric, Rng};
use rccell::RcCell;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::iter::FromIterator;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use write_to::ReadFrom;

use bit_vec::BitVec;
use log::{debug, info, warn};
use mio::net::{TcpListener, TcpStream, UdpSocket};
use mio::{Interest, Poll, Token};
use std::net::SocketAddr;

use super::block_cache::BlockCache;
use super::disk_manager::{ConnectionIdentifier, DiskRequest};
use super::piece_info::PieceInfo;
use super::tracker_manager::TrackerRequest;
use super::{piece_assigner::PieceAssigner, HandshakingConnection};
use super::{
    Connection, EstablishedConnection, HandshakeUpdateSuccess, UpdateError, UpdateSuccess,
};
use crate::client::connection::ConnectionBase;
use crate::client::utp::{
    EstablishedUtpConnection, HandshakingUpdateSuccess, Header, IncomingUtpConnection,
    OutgoingUtpConnection, UtpConnection, UtpSocket,
};
use crate::common::PEER_ID_PREFIX;
use crate::common::{MetaInfo, SharedBlockCache, SharedCount, PEER_ID_LENGTH};
use crate::common::{Sha1Hash, SharedPieceAssigner};
use crate::io::ReadBuffer;
use crate::messages::{Bitfield, Block, Handshake, Have, Interested, Unchoke};
use crate::tracker::{EventKind, PeerInfo, PeerInfoList};

const PRINT_UPDATE_TIME: std::time::Duration = std::time::Duration::from_secs(1);

pub struct ConnectionManager {
    connections: HashMap<Token, Connection>,
    downloaded: usize,
    uploaded: usize,
    last_update: std::time::Instant,
    next_socket_index: usize,
    torrents: HashMap<Sha1Hash, TorrentData>,
    config: ConnectionManagerConfig,
    listener: TcpListener,
    peer_id: [u8; PEER_ID_LENGTH],
    tracker_sender: Sender<TrackerRequest>,
    token_to_info_hash: HashMap<Token, Sha1Hash>,
    addr_to_info_hash: HashMap<SocketAddr, Sha1Hash>,
    read_buffer: ReadBuffer,
    poll: RcCell<Poll>,
    utp_socket: Rc<UdpSocket>,
    utp_connections: HashMap<SocketAddr, UtpConnection>,
}

pub struct ConnectionManagerConfig {
    pub listen_port: u16,
    pub max_peers: usize,
    pub seed: bool,
    pub print_output: bool,
}

pub struct TorrentData {
    info_hash: Sha1Hash,
    piece_assigner: SharedPieceAssigner,
    downloaded: SharedCount,
    uploaded: SharedCount,
    piece_info: PieceInfo,
    block_cache: SharedBlockCache,
    have_on_disk: BitVec,
    disk_requester: Sender<DiskRequest>,
    peers_data: PeersData,
}

// Token to info_hash map

pub struct PeersData {
    connected_id: HashSet<[u8; PEER_ID_LENGTH]>,
    token_to_info: HashMap<Token, PeerInfo>,
    connected_socketaddr: HashSet<SocketAddr>,
    unconnected: Vec<PeerInfo>,
}

// Need to query by both Token and PeerId
// iter connections?  Shared ptr to connections?  Replace connections in manager with something like this?
impl PeersData {
    pub fn new() -> Self {
        Self {
            connected_id: HashSet::new(),
            token_to_info: HashMap::new(),
            connected_socketaddr: HashSet::new(),
            unconnected: Vec::new(),
        }
    }

    pub fn add_peers_from_tracker(&mut self, peers: Vec<PeerInfo>) {
        for peer in peers {
            if let Some(id) = peer.id {
                if !self.connected_id.contains(&id) {
                    self.unconnected.push(peer);
                }
            } else if !self.connected_socketaddr.contains(&peer.addr) {
                self.unconnected.push(peer);
            }
        }
    }

    pub fn add_connected_id(&mut self, token: Token, id: [u8; PEER_ID_LENGTH], addr: SocketAddr) {
        let peer_info = self
            .token_to_info
            .entry(token)
            .or_insert(PeerInfo { addr, id: None });
        peer_info.id = Some(id);
        self.connected_id.insert(id);
    }

    pub fn get_unconnected(&mut self, amount: usize) -> Vec<PeerInfo> {
        let drain_amount = std::cmp::min(amount, self.unconnected.len());
        self.unconnected.drain(..drain_amount).collect()
    }

    pub fn have_unconnected(&self) -> bool {
        !self.unconnected.is_empty()
    }

    pub fn is_connected(&self, peer_id: &[u8; PEER_ID_LENGTH]) -> bool {
        self.connected_id.contains(peer_id)
    }

    pub fn connected(&mut self, token: Token, peer_info: PeerInfo) {
        if let Some(id) = peer_info.id {
            self.connected_id.insert(id);
        }
        self.connected_socketaddr.insert(peer_info.addr);
        self.token_to_info.insert(token, peer_info);
    }

    pub fn disconnected(&mut self, token: Token) {
        let PeerInfo { addr, id } = self.token_to_info[&token];
        if let Some(id) = id {
            self.connected_id.remove(&id);
        }
        self.connected_socketaddr.remove(&addr);
        self.token_to_info.remove(&token);
    }
}

enum PromotionEvent {
    CompletedHandshake(Handshake),
}

impl ConnectionManager {
    pub fn new(
        config: ConnectionManagerConfig,
        poll: RcCell<Poll>,
        listener: TcpListener,
        utp_socket: UdpSocket,
        tracker_sender: Sender<TrackerRequest>,
    ) -> Self {
        let peer_id_suffix = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(PEER_ID_LENGTH - PEER_ID_PREFIX.len())
            .map(char::from);
        let mut peer_id = PEER_ID_PREFIX.to_owned();
        peer_id.extend(peer_id_suffix);
        ConnectionManager {
            connections: HashMap::new(),
            downloaded: 0,
            uploaded: 0,
            last_update: std::time::Instant::now(),
            next_socket_index: 0,
            torrents: HashMap::new(),
            config,
            listener,
            peer_id: peer_id.into_bytes().try_into().unwrap(),
            tracker_sender,
            token_to_info_hash: HashMap::new(),
            read_buffer: ReadBuffer::new(1 << 20), // 1 MiB
            poll,
            utp_socket: Rc::new(utp_socket),
            utp_connections: HashMap::new(),
            addr_to_info_hash: HashMap::new(),
        }
    }

    pub fn reregister_connected(&mut self, token: Token) {
        if let Some(Connection::Handshaking(peer)) = self.connections.get_mut(&token) {
            peer.reregister(&self.poll.borrow(), token, Interest::READABLE);
        }
    }

    pub fn start_torrent(
        &mut self,
        meta_info: MetaInfo,
        piece_have: BitVec,
        disk_sender: Sender<DiskRequest>,
    ) {
        let num_pieces = meta_info.pieces.len();
        let piece_length = meta_info.piece_length;
        info!("Piece length: {}, Num pieces: {}", piece_length, num_pieces);
        let piece_info = PieceInfo {
            piece_length,
            total_pieces: num_pieces,
            total_length: meta_info.total_size,
        };
        let piece_assigner = Rc::new(RefCell::new(PieceAssigner::new(piece_info, &piece_have)));
        // Create shared Block cache that connections will write to
        let block_cache = Rc::new(RefCell::new(BlockCache::new(
            meta_info.info_hash_raw,
            piece_info,
            piece_have.clone(),
            disk_sender.clone(),
            meta_info.pieces.clone(),
        )));
        let info_hash = meta_info.info_hash_raw;
        self.tracker_sender
            .send(TrackerRequest::Register {
                info_hash,
                announce_url: meta_info.announce,
                peer_id: self.peer_id,
                listen_port: self.config.listen_port,
            })
            .unwrap();
        self.tracker_sender
            .send(TrackerRequest::Announce {
                info_hash,
                upload: 0,
                download: 0,
                left: meta_info.pieces.len(),
                event: EventKind::Started,
            })
            .unwrap();
        let torrent_data = TorrentData {
            info_hash,
            piece_assigner,
            downloaded: Rc::new(RefCell::new(0)),
            uploaded: Rc::new(RefCell::new(0)),
            piece_info,
            block_cache,
            have_on_disk: piece_have,
            disk_requester: disk_sender,
            peers_data: PeersData::new(),
        };
        self.torrents.insert(info_hash, torrent_data);
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
                        .register(&mut stream, token, Interest::READABLE)
                        .unwrap();
                    let peer = HandshakingConnection::new_from_incoming(
                        Box::new(stream),
                        token,
                        self.peer_id,
                    );
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

    pub fn handle_utp_event(&mut self) {
        loop {
            if let Err(error) = self.handle_utp_event_inner() {
                // TODO: add UTP "disconnect" if not WOULD_BLOCK
                if let UpdateError::CommunicationError(error) = &error {
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        break;
                    }
                }
                warn!("utp error: {:?}", error);
                // self.disconnect_peer(token, error);
                break;
            }
        }
    }

    pub fn handle_utp_event_inner(&mut self) -> Result<(), UpdateError> {
        const MAX_UDP_PACKET_SIZE: usize = 65507;
        self.read_buffer.clear();
        // read header and optional message
        if self.read_buffer.unused() < MAX_UDP_PACKET_SIZE {
            self.read_buffer.shift_left();
        }
        let buffer = self.read_buffer.get_unused_mut();
        let (read, addr) = self.utp_socket.recv_from(buffer)?;
        debug!("Received packet of size {}", read);
        self.read_buffer.added_unused(read);

        if read != 20 && self.read_buffer.get_unread()[0] == "d".as_bytes()[0] {
            info!("message is DHT info, ignoring");
            self.read_buffer.clear();
            return Ok(());
        }

        let (header, remaining) = Header::read_from(&mut self.read_buffer, read)?;
        debug!(
            "Received header from {:?}: {:?}: remaining {}: {:?}",
            addr,
            header.get_type(),
            remaining,
            header
        );
        if header.get_type() == crate::client::utp::Type::StFin {
            debug!(
                "ST_FIN received, remaining: {:?}",
                self.read_buffer.get_unread()
            );
        }
        // TODO: the key is not really Addr, it is connection id.
        let event = match self.utp_connections.entry(addr).or_insert_with(|| {
            UtpConnection::Incoming(IncomingUtpConnection::new(
                UtpSocket::new_from_incoming(self.utp_socket.clone(), addr, &header),
                self.peer_id,
            ))
        }) {
            UtpConnection::Incoming(conn) => {
                // assert_eq!(0, remaining); // Expecting only headers
                if let HandshakingUpdateSuccess::Complete(handshake) =
                    conn.update(&mut self.read_buffer, &header)?
                {
                    PromotionEvent::CompletedHandshake(handshake)
                } else {
                    return Ok(());
                }
            }
            UtpConnection::Outgoing(conn) => {
                if let HandshakingUpdateSuccess::Complete(handshake) =
                    conn.update(&mut self.read_buffer, &header)?
                {
                    PromotionEvent::CompletedHandshake(handshake)
                } else {
                    return Ok(());
                }
            }
            UtpConnection::Established(conn) => {
                match conn.update(&mut self.read_buffer, &header)? {
                    UpdateSuccess::Transferred {
                        downloaded,
                        uploaded,
                    } => {
                        self.downloaded += downloaded;
                        self.uploaded += uploaded;
                    }
                    UpdateSuccess::NoUpdate | UpdateSuccess::Success => {}
                }
                return Ok(());
            }
        };
        match event {
            PromotionEvent::CompletedHandshake(handshake) => {
                let torrent_data =
                    if let Some(torrent_data) = self.torrents.get_mut(&handshake.info_hash) {
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
                let peer = self.utp_connections.remove(&addr).unwrap();
                let socket = peer.promote();
                self.addr_to_info_hash.insert(addr, handshake.info_hash);
                // torrent_data // TODO figure out if I need to do this
                //     .peers_data
                //     .add_connected_id(token, handshake.peer_id, addr);
                let mut promoted = EstablishedUtpConnection::new(
                    0, // Add UTP Ids
                    torrent_data.info_hash,
                    socket,
                    torrent_data.piece_info.total_pieces,
                    torrent_data.piece_assigner.clone(),
                    torrent_data.disk_requester.clone(),
                    torrent_data.block_cache.clone(),
                    torrent_data.downloaded.clone(),
                    torrent_data.uploaded.clone(),
                );
                // Send this in one ST_DATA message
                promoted.send(&Bitfield {
                    bitfield: torrent_data.have_on_disk.clone(),
                })?;
                promoted.send(&Unchoke {})?;
                promoted.send(&Interested {})?;
                // Call update in case we have more data with the handshake
                promoted.update(&mut self.read_buffer, &header)?;
                self.utp_connections
                    .insert(addr, UtpConnection::Established(promoted));
            }
        }
        Ok(())
    }

    pub fn handle_event(&mut self, token: Token) {
        if let Err(error) = self.handle_event_inner(token) {
            // If it was a TCP connnection, try again on UDP?
            self.disconnect_peer(token, error);
        }
    }

    pub fn handle_event_inner(&mut self, token: Token) -> Result<(), UpdateError> {
        let peer = if let Some(peer) = self.connections.get_mut(&token) {
            peer
        } else {
            return Ok(());
        };
        match peer {
            // Connection::Initiating(_) => todo!(),
            Connection::Handshaking(connection) => {
                match connection.update(&mut self.read_buffer)? {
                    HandshakeUpdateSuccess::NoUpdate => {}
                    HandshakeUpdateSuccess::Complete(handshake) => {
                        self.promote(token, handshake)?
                    }
                }
            }
            Connection::Established(connection) => {
                match connection.update(&mut self.read_buffer)? {
                    UpdateSuccess::Transferred {
                        downloaded,
                        uploaded,
                    } => {
                        self.downloaded += downloaded;
                        self.uploaded += uploaded;
                    }
                    UpdateSuccess::NoUpdate | UpdateSuccess::Success => {}
                }
            }
        };
        Ok(())
    }

    fn promote(&mut self, token: Token, handshake: Handshake) -> Result<(), UpdateError> {
        debug!("Handshake received, promoting to established connection");
        let torrent_data = if let Some(torrent_data) = self.torrents.get_mut(&handshake.info_hash) {
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
                token.0,
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
        self.handle_event_inner(token)
    }

    pub fn disconnect_peer(&mut self, token: Token, error: UpdateError) -> Option<SocketAddr> {
        info!("Removing peer {}: {} while updating", token.0, error);
        if let Some(info_hash) = self.token_to_info_hash.remove(&token) {
            if let Some(torrent_data) = self.torrents.get_mut(&info_hash) {
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

    pub fn add_peers(&mut self, info_hash: Sha1Hash, peer_list: PeerInfoList) {
        let torrent = if let Some(torrent) = self.torrents.get_mut(&info_hash) {
            torrent
        } else {
            warn!(
                "Requested add_peers for torrent that is not managed: {:?}",
                info_hash
            );
            return;
        };
        torrent.peers_data.add_peers_from_tracker(peer_list);
        self.connect_peers(info_hash);
    }

    pub fn connect_peers(&mut self, info_hash: Sha1Hash) {
        let torrent = if let Some(torrent) = self.torrents.get_mut(&info_hash) {
            torrent
        } else {
            warn!(
                "Requested connect_peers for torrent that is not managed: {:?}",
                info_hash
            );
            return;
        };
        let free = self.config.max_peers - self.connections.len();
        if free <= 0 || !torrent.peers_data.have_unconnected() {
            return;
        }
        let peer_list = torrent.peers_data.get_unconnected(free);
        for peer_info in peer_list {
            debug!("Attempting connection to peer: {:?}", peer_info);
            // have setting for trying UDP connection first
            if false {
                // TCP Connection
                let mut stream = match TcpStream::connect(peer_info.addr) {
                    Ok(stream) => stream,
                    Err(error) => {
                        info!("Unable to connect to {}: {:?}", peer_info.addr, error);
                        continue;
                    }
                };
                let token = Token(self.next_socket_index);
                torrent.peers_data.connected(token, peer_info.clone());
                self.next_socket_index += 1;
                // Registering the socket for Writable notifications will tell us when it is connected.
                self.poll
                    .borrow()
                    .registry()
                    .register(&mut stream, token, Interest::WRITABLE)
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
            } else {
                // Utp connection
                let peer = match OutgoingUtpConnection::new(
                    UtpSocket::new(self.utp_socket.clone(), peer_info.addr),
                    torrent.info_hash,
                    self.peer_id,
                ) {
                    Ok(peer) => UtpConnection::Outgoing(peer),
                    Err(error) => {
                        warn!("Failed to connect to peer: {:?}", error);
                        continue;
                    }
                };
                // TODO torrent.peers_data.connected(token, peer_info.clone());
                self.utp_connections.insert(peer_info.addr, peer);
            }
        }
    }

    pub fn maybe_print_info(&mut self) {
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
                let have = &torrent.have_on_disk;
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
                println!("{progress_bar}");
                println!(
                    "Percent done: {:.2}% Download: {:.2} MiB/s Upload: {:.2} MiB/s Peers: {}",
                    torrent.block_cache.borrow().percent_done(),
                    download_rate,
                    upload_rate,
                    self.connections.len(),
                );
                info!("Download: {:.2} MiB/s", download_rate);
                let mut temp = Vec::new();
                for (k, v) in self.connections.iter() {
                    temp.push((k, v));
                }
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
    pub fn send_have(&mut self, info_hash: Sha1Hash, piece_index: usize) {
        let mut to_remove = Vec::new();
        if let Some(torrent) = self.torrents.get_mut(&info_hash) {
            torrent.have_on_disk.set(piece_index, true);
            for (token, _) in &torrent.peers_data.token_to_info {
                if let Some(connection) = self.connections.get_mut(token) {
                    if let Connection::Established(peer) = connection {
                        if peer.peer_has[piece_index] {
                            continue;
                        }
                        if let Err(error) = peer.send(&Have {
                            index: piece_index as u32,
                        }) {
                            info!("Disconnecting peer {}: {} while sending", token.0, error);
                            to_remove.push((*token, error));
                        }
                    }
                }
            }
        }
        for (token, error) in to_remove {
            self.disconnect_peer(token, error.into());
        }
    }

    pub fn send_block(
        &mut self,
        _info_hash: Sha1Hash,
        conn_id: ConnectionIdentifier,
        block: Block,
    ) {
        match conn_id {
            ConnectionIdentifier::TcpToken(token) => {
                let error = if let Some(Connection::Established(connection)) =
                    self.connections.get_mut(&token)
                {
                    if let Err(error) = connection.send(&block) {
                        error
                    } else {
                        return;
                    }
                } else {
                    return;
                };
                self.disconnect_peer(token, error.into());
            }
            ConnectionIdentifier::UtpId(addr) => {
                let error = if let Some(UtpConnection::Established(conn)) =
                    self.utp_connections.get_mut(&addr)
                {
                    if let Err(error) = conn.send(&block) {
                        error
                    } else {
                        return;
                    }
                } else {
                    return;
                };
                warn!("Error from sending UTP block: {:?}", error);
                // "disconnect" UTP peer?
            }
        }
    }
}
