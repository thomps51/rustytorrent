use rand::{distributions::Alphanumeric, Rng};
use rccell::RcCell;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::io::Write;
use std::iter::FromIterator;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use write_to::{ReadFrom, WriteTo};

use bit_vec::BitVec;
use log::{debug, info, warn};
use mio::net::{TcpListener, TcpStream, UdpSocket};
use mio::{Interest, Poll, Token};
use std::net::SocketAddr;

use super::block_cache::BlockCache;
use super::disk_manager::{ConnectionIdentifier, DiskRequest};
use super::piece_info::PieceInfo;
use super::tracker_manager::TrackerRequest;
use super::utp::Type;
use super::{piece_assigner::PieceAssigner, HandshakingConnection};
use super::{
    Connection, EstablishedConnection, HandshakeUpdateSuccess, UpdateError, UpdateSuccess,
};
use crate::client::connection::ConnectionBase;
use crate::client::utp::{
    EstablishedUtpConnection, Header, IncomingUtpConnection, OutgoingUtpConnection, UtpConnection,
    UtpConnectionInfo,
};
use crate::common::PEER_ID_PREFIX;
use crate::common::{MetaInfo, SharedBlockCache, SharedCount, PEER_ID_LENGTH};
use crate::common::{Sha1Hash, SharedPieceAssigner};
use crate::io::recvmmsg::{PacketData, UtpReceiver};
use crate::io::sendmmsg::UtpBlockSender;
use crate::io::ReadBuffer;
use crate::messages::{Bitfield, Block, Handshake, Have, Interested, ProtocolMessage, Unchoke};
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
    addr_to_info_hash: HashMap<(SocketAddr, u16), Sha1Hash>,
    read_buffer: ReadBuffer,
    utp_send_buffer: UtpSendBuffer,
    poll: RcCell<Poll>,
    utp_socket: Rc<UdpSocket>,
    utp_connections: HashMap<(SocketAddr, u16), UtpConnection>,
    utp_block_sender: UtpBlockSender,
    utp_receiver: UtpReceiver,
}

#[derive(Default)]
pub struct UtpSendBuffer {
    data_buffer: Vec<u8>,
    header_only: Vec<Type>,
    header_buffer: [u8; Header::SIZE],
}

impl UtpSendBuffer {
    pub fn add_message<T: ProtocolMessage>(&mut self, message: T) {
        message
            .write(&mut self.data_buffer)
            .expect("vec write can't fail");
    }

    pub fn add_data<T: WriteTo>(&mut self, data: T) {
        data.write_to(&mut self.data_buffer)
            .expect("vec write can't fail");
    }

    pub fn add_header(&mut self, header: Type) {
        self.header_only.push(header);
    }

    pub fn send(
        &mut self,
        utp_sender: &mut UtpBlockSender,
        socket: &UdpSocket,
        conn_info: &mut UtpConnectionInfo,
    ) -> std::io::Result<()> {
        for header_type in self.header_only.drain(..) {
            let header = conn_info.create_header(header_type);
            header
                .write_to(&mut self.header_buffer.as_mut_slice())
                .unwrap();
            socket.send_to(&self.header_buffer, conn_info.addr())?;
        }
        if self.data_buffer.is_empty() {
            return Ok(());
        }
        let header = conn_info.create_header(Type::StData);
        // TODO: get packet size from conn_info
        let sent = utp_sender.send(
            header,
            socket,
            1000,
            &mut self.data_buffer,
            conn_info.addr(),
        )?;
        self.data_buffer.clear();
        // sent - 1 because create_header adds one (TODO: This smells)
        let value = conn_info.seq_nr.wrapping_add((sent - 1) as u16);
        conn_info.seq_nr = value;
        Ok(())
    }
}

impl Write for UtpSendBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.data_buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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

#[derive(Default)]
pub struct PeersData {
    connected_id: HashSet<[u8; PEER_ID_LENGTH]>,
    token_to_info: HashMap<Token, PeerInfo>,
    utp_connected: HashMap<(SocketAddr, u16), [u8; PEER_ID_LENGTH]>,
    connected_socketaddr: HashSet<SocketAddr>,
    unconnected: Vec<PeerInfo>,
}

// Need to query by both Token and PeerId
// iter connections?  Shared ptr to connections?  Replace connections in manager with something like this?
impl PeersData {
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

    pub fn add_connected_id_utp(
        &mut self,
        connection_id: u16,
        id: [u8; PEER_ID_LENGTH],
        addr: SocketAddr,
    ) {
        self.connected_id.insert(id);
        self.utp_connected.insert((addr, connection_id), id);
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

    pub fn connected_utp(&mut self, _id: u16, peer_info: PeerInfo) {
        if let Some(id) = peer_info.id {
            self.connected_id.insert(id);
        }
        self.connected_socketaddr.insert(peer_info.addr);
    }

    pub fn disconnected(&mut self, token: Token) {
        let PeerInfo { addr, id } = self.token_to_info[&token];
        if let Some(id) = id {
            self.connected_id.remove(&id);
        }
        self.connected_socketaddr.remove(&addr);
        self.token_to_info.remove(&token);
    }

    pub fn disconnected_utp(&mut self, addr: SocketAddr, id: u16) {
        let id = self.utp_connected.remove(&(addr, id));
        if let Some(id) = id {
            self.connected_id.remove(&id);
        }
        self.connected_socketaddr.remove(&addr);
    }
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
        use sysctl::Sysctl;
        let max_udp_packet_size =
            match sysctl::Ctl::new("net.inet.udp.maxdgram").map(|x| x.value_as::<u32>()) {
                Ok(Ok(value)) => *value as usize,
                _ => 65507,
            };
        let num_read_buffers = (1 << 20) / max_udp_packet_size;

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
            utp_block_sender: UtpBlockSender::new(),
            utp_receiver: UtpReceiver::new(num_read_buffers, max_udp_packet_size),
            utp_send_buffer: Default::default(),
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
            peers_data: Default::default(),
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
        let mut num_packets = 0;
        let mut to_disconnect = vec![];
        for PacketData {
            buffer: ref mut read_buffer,
            ref addr,
        } in self.utp_receiver.recv(&self.utp_socket)?
        {
            num_packets += 1;
            let read = read_buffer.unread();
            if read != 20 && read_buffer.get_unread()[0] == "d".as_bytes()[0] {
                info!("message is DHT info, ignoring");
                continue;
            }

            let (header, remaining) = Header::read_from(read_buffer, read)?;
            debug!(
                "Received header from {:?}: {:?}: remaining {}: {:?}",
                addr,
                header.get_type(),
                remaining,
                header
            );
            // Ugly parameter list to avoid borrow checker complaints...
            match Self::handle_utp_event_more_inner(
                read_buffer,
                &header,
                addr,
                &mut self.utp_connections,
                self.peer_id,
                &mut self.torrents,
                &mut self.addr_to_info_hash,
                &mut self.utp_send_buffer,
            ) {
                Ok((addr, connection_id)) => {
                    // TODO: optimize this lookup, it isn't necessary size we already did it in handle_utp_event_more_inner
                    let connection = self
                        .utp_connections
                        .get_mut(&(addr, connection_id))
                        .unwrap();
                    if let Err(error) = self.utp_send_buffer.send(
                        &mut self.utp_block_sender,
                        &self.utp_socket,
                        connection.connection_info(),
                    ) {
                        warn!("Failed to connect to peer: {:?}", error);
                        continue;
                    }
                }
                Err(error) => {
                    warn!("utp error: {:?}", error);
                    if let Ok(addr) = addr {
                        to_disconnect.push((*addr, header.connection_id, error));
                    }
                    // TODO: disconnect and ST_FIN
                    continue;
                }
            }
        }
        for (addr, id, error) in to_disconnect {
            self.disconnect_peer_utp(addr, id, error);
        }
        if num_packets < self.utp_receiver.num_buffers() {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        Ok(())
    }

    pub fn handle_utp_event_more_inner(
        read_buffer: &mut ReadBuffer,
        header: &Header,
        addr: &std::io::Result<SocketAddr>,
        utp_connections: &mut HashMap<(SocketAddr, u16), UtpConnection>,
        peer_id: [u8; PEER_ID_LENGTH],
        torrents: &mut HashMap<Sha1Hash, TorrentData>,
        addr_to_info_hash: &mut HashMap<(SocketAddr, u16), Sha1Hash>,
        utp_send_buffer: &mut UtpSendBuffer,
    ) -> Result<(SocketAddr, u16), UpdateError> {
        if header.get_type() == crate::client::utp::Type::StFin {
            debug!("ST_FIN received, remaining: {:?}", read_buffer.get_unread());
        }
        // If connection id is not in connections, add one to it when we insert it per the spec
        let connection_id = header.connection_id;
        let addr = *addr.as_ref()?;
        let (connection, connection_id) = match utp_connections.entry((addr, connection_id)) {
            std::collections::hash_map::Entry::Occupied(v) => (v.into_mut(), connection_id),
            std::collections::hash_map::Entry::Vacant(_) => {
                let connection_id = connection_id + 1;
                (
                    utp_connections
                        .entry((addr, connection_id))
                        .or_insert_with(|| {
                            UtpConnection::Incoming(IncomingUtpConnection::new(
                                UtpConnectionInfo::new_from_incoming(addr, header),
                                peer_id,
                            ))
                        }),
                    connection_id,
                )
            }
        };
        let Some(handshake) = connection.update(header, read_buffer, utp_send_buffer)? else {
            return Ok((addr, connection_id))
        };

        let Some(torrent_data) = torrents.get_mut(&handshake.info_hash) else {
            return Err(UpdateError::TorrentNotManaged {
                info_hash: handshake.info_hash,
            });
        };
        if torrent_data.peers_data.is_connected(&handshake.peer_id) || handshake.peer_id == peer_id
        {
            return Err(UpdateError::CommunicationError(
                std::io::ErrorKind::AlreadyExists.into(),
            ));
        }
        let peer = utp_connections.remove(&(addr, connection_id)).unwrap();
        let socket = peer.promote();
        addr_to_info_hash.insert((addr, header.connection_id), handshake.info_hash);
        torrent_data
            .peers_data
            .add_connected_id_utp(socket.conn_id_recv, handshake.peer_id, addr);
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
        utp_send_buffer.add_message(Bitfield {
            bitfield: torrent_data.have_on_disk.clone(),
        });
        utp_send_buffer.add_message(Unchoke {});
        utp_send_buffer.add_message(Interested {});
        // Call update in case we have more data with the handshake
        // Actually required even when unread is zero..., why is this? Has something to do with
        // Phantom ACK showing up in outgoing.
        // if read_buffer.unread() > 0 {
        promoted.update(read_buffer, header, utp_send_buffer)?;
        // }
        utp_connections.insert((addr, connection_id), UtpConnection::Established(promoted));
        Ok((addr, connection_id))
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

    pub fn disconnect_peer_utp(
        &mut self,
        addr: SocketAddr,
        id: u16,
        error: UpdateError,
    ) -> Option<SocketAddr> {
        info!("Removing peer {}, {}: {} while updating", addr, id, error);
        if let Some(info_hash) = self.addr_to_info_hash.remove(&(addr, id)) {
            if let Some(torrent_data) = self.torrents.get_mut(&info_hash) {
                torrent_data.peers_data.disconnected_utp(addr, id);
            }
        }
        if let Some(_connection) = self.utp_connections.remove(&(addr, id)) {
            return Some(addr);
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
        let free = self.config.max_peers.saturating_sub(self.connections.len());
        if free == 0 || !torrent.peers_data.have_unconnected() {
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
                let mut connection = OutgoingUtpConnection::new(
                    UtpConnectionInfo::new(peer_info.addr),
                    torrent.info_hash,
                    self.peer_id,
                );
                self.utp_send_buffer.add_header(Type::StSyn);
                let connection_id = connection.socket.conn_id_recv;
                if let Err(error) = self.utp_send_buffer.send(
                    &mut self.utp_block_sender,
                    &self.utp_socket,
                    &mut connection.socket,
                ) {
                    warn!("Failed to connect to peer: {:?}", error);
                    continue;
                }
                torrent
                    .peers_data
                    .connected_utp(connection_id, peer_info.clone());
                self.utp_connections.insert(
                    (peer_info.addr, connection_id),
                    UtpConnection::Outgoing(connection),
                );
            }
        }
    }

    pub fn maybe_print_info(&mut self) {
        if !self.config.print_output {
            return;
        }
        for torrent in self.torrents.values() {
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
                std::io::stdout().flush().expect("Could not flush stdout");
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
            for token in torrent.peers_data.token_to_info.keys() {
                if let Some(Connection::Established(peer)) = self.connections.get_mut(token) {
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
            ConnectionIdentifier::UtpId(addr, conn_id) => {
                let Some(UtpConnection::Established(conn)) =
                    self.utp_connections.get_mut(&(addr, conn_id)) else {return};

                let utp_header = conn.get_utp_header();
                match self.utp_block_sender.send_block(
                    utp_header,
                    self.utp_socket.as_ref(),
                    1000,
                    block,
                    addr,
                ) {
                    // get_utp_header will actually add 1 to seq_nr, so we subtract one here.
                    // probably should change that though.
                    Ok(sent) => conn.add_seq_nr(sent - 1),
                    Err(error) => {
                        warn!("Error from sending UTP block: {:?}", error);
                        // "disconnect" UTP peer? Send FIN
                    }
                }
            }
        }
    }
}
