use rand::{distributions::Alphanumeric, Rng};
use rccell::RcCell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::iter::FromIterator;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use bit_vec::BitVec;
use log::{debug, info, warn};
use mio::net::{TcpListener, UdpSocket};
use mio::{Poll, Token};

use super::block_cache::BlockCache;
use super::disk_manager::{ConnectionIdentifier, DiskRequest};
use super::peers_data::PeersData;
use super::piece_assigner::PieceAssigner;
use super::piece_info::PieceInfo;
use super::tcp::connection_manager::TcpConnectionManager;
use super::tracker_manager::TrackerRequest;
use super::utp::connection_manager::UtpConnectionManager;
use crate::common::{MetaInfo, SharedBlockCache, SharedCount, PEER_ID_LENGTH, PEER_ID_PREFIX};
use crate::common::{Sha1Hash, SharedPieceAssigner};
use crate::messages::Block;
use crate::tracker::{EventKind, PeerInfoList};

const PRINT_UPDATE_TIME: std::time::Duration = std::time::Duration::from_secs(1);

pub struct ConnectionManager {
    downloaded: usize,
    uploaded: usize,
    last_update: std::time::Instant,
    torrents: HashMap<Sha1Hash, TorrentData>,
    config: ConnectionManagerConfig,
    peer_id: [u8; PEER_ID_LENGTH],
    tracker_sender: Sender<TrackerRequest>,
    tcp_manager: TcpConnectionManager,
    utp_socket: UdpSocket,
    utp_manager: UtpConnectionManager,
}

pub struct ConnectionManagerConfig {
    pub listen_port: u16,
    pub max_peers: usize,
    pub seed: bool,
    pub print_output: bool,
}

pub struct TorrentData {
    pub info_hash: Sha1Hash,
    pub piece_assigner: SharedPieceAssigner,
    pub downloaded: SharedCount,
    pub uploaded: SharedCount,
    pub piece_info: PieceInfo,
    pub block_cache: SharedBlockCache,
    pub have_on_disk: BitVec,
    pub disk_requester: Sender<DiskRequest>,
    pub peers_data: PeersData,
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
        let peer_id: [u8; PEER_ID_LENGTH] = peer_id.into_bytes().try_into().unwrap();

        Self {
            downloaded: 0,
            uploaded: 0,
            last_update: std::time::Instant::now(),
            torrents: HashMap::new(),
            config,
            peer_id,
            tracker_sender,
            utp_socket,
            utp_manager: UtpConnectionManager::new(peer_id),
            tcp_manager: TcpConnectionManager::new(peer_id, listener, poll),
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

    pub fn handle_utp_event(&mut self) {
        self.utp_manager
            .handle_utp_event(&mut self.torrents, &self.utp_socket)
    }

    pub fn handle_event(&mut self, token: Token) {
        self.tcp_manager.handle_event(&mut self.torrents, token)
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
        let free = self.config.max_peers.saturating_sub(
            self.utp_manager.num_connections() + self.tcp_manager.num_connections(),
        );
        if free == 0 || !torrent.peers_data.have_unconnected() {
            return;
        }
        let peer_list = torrent.peers_data.get_unconnected(free);
        for peer_info in peer_list {
            debug!("Attempting connection to peer: {:?}", peer_info);
            // have setting for trying UDP connection first
            if false {
                self.tcp_manager.connect_to(torrent, peer_info);
            } else {
                self.utp_manager
                    .connect_to(torrent, peer_info, &self.utp_socket);
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
                    "Percent done: {:.2}% Download: {:.2} MiB/s Upload: {:.2} MiB/s",
                    torrent.block_cache.borrow().percent_done(),
                    download_rate,
                    upload_rate,
                );
                info!("Download: {:.2} MiB/s", download_rate);
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
        self.tcp_manager
            .send_have(&mut self.torrents, info_hash, piece_index);
        self.utp_manager
            .send_have(&mut self.torrents, info_hash, piece_index, &self.utp_socket)
    }

    pub fn send_block(&mut self, conn_id: ConnectionIdentifier, block: Block) {
        match conn_id {
            ConnectionIdentifier::TcpToken(token) => {
                self.tcp_manager
                    .send_block(&mut self.torrents, block, token);
            }
            ConnectionIdentifier::UtpId(addr, conn_id) => {
                self.utp_manager
                    .send_block(addr, conn_id, block, &self.utp_socket);
            }
        }
    }

    pub fn accept_tcp_connections(&mut self) {
        self.tcp_manager.accept_connections()
    }
}
