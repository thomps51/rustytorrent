use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
};

use mio::Token;

use crate::{common::PEER_ID_LENGTH, tracker::PeerInfo};

#[derive(Default)]
pub struct PeersData {
    connected_id: HashSet<[u8; PEER_ID_LENGTH]>,
    pub token_to_info: HashMap<Token, PeerInfo>,
    pub utp_connected: HashMap<(SocketAddr, u16), [u8; PEER_ID_LENGTH]>,
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
