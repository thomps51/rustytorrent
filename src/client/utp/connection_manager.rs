use std::{collections::HashMap, net::SocketAddr};

use log::{debug, info, warn};
use mio::net::UdpSocket;
use write_to::ReadFrom;

use crate::{
    client::{connection_manager::TorrentData, utp::Header, UpdateError},
    common::{Sha1Hash, PEER_ID_LENGTH},
    io::{
        recvmmsg::{PacketData, UtpReceiver},
        sendmmsg::UtpBlockSender,
        ReadBuffer,
    },
    messages::{Bitfield, Block, Interested, Unchoke},
    tracker::PeerInfo,
};

use super::{
    EstablishedUtpConnection, IncomingUtpConnection, OutgoingUtpConnection, Type, UtpConnection,
    UtpConnectionInfo, UtpSendBuffer,
};

#[derive(Default)]
pub struct UtpConnectionManager {
    addr_to_info_hash: HashMap<(SocketAddr, u16), Sha1Hash>,
    utp_send_buffer: UtpSendBuffer,
    utp_connections: HashMap<(SocketAddr, u16), UtpConnection>,
    utp_block_sender: UtpBlockSender,
    utp_receiver: UtpReceiver,
    peer_id: [u8; PEER_ID_LENGTH],
}

impl UtpConnectionManager {
    pub fn new(peer_id: [u8; PEER_ID_LENGTH]) -> Self {
        use sysctl::Sysctl;
        let max_udp_packet_size =
            match sysctl::Ctl::new("net.inet.udp.maxdgram").map(|x| x.value_as::<u32>()) {
                Ok(Ok(value)) => *value as usize,
                _ => 65507,
            };
        let num_read_buffers = (1 << 20) / max_udp_packet_size;
        Self {
            peer_id,
            utp_receiver: UtpReceiver::new(num_read_buffers, max_udp_packet_size),
            ..Default::default()
        }
    }

    pub fn handle_utp_event(
        &mut self,
        torrents: &mut HashMap<Sha1Hash, TorrentData>,
        utp_socket: &UdpSocket,
    ) {
        loop {
            match self.handle_utp_event_inner(torrents, utp_socket) {
                Ok(num_packets_read) => {
                    if num_packets_read < self.utp_receiver.num_buffers() {
                        break;
                    }
                }
                Err(error) => {
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        break;
                    }
                    warn!("utp error: {:?}", error);
                    break;
                }
            }
        }
    }

    pub fn handle_utp_event_inner(
        &mut self,
        torrents: &mut HashMap<Sha1Hash, TorrentData>,
        utp_socket: &UdpSocket,
    ) -> std::io::Result<usize> {
        let mut to_disconnect = vec![];
        let (num_packets, packets) = self.utp_receiver.recv(utp_socket)?;
        for PacketData {
            buffer: ref mut read_buffer,
            ref addr,
        } in packets
        {
            let read = read_buffer.unread();
            // TODO: I need a better way of handling DHT info
            if read != 20 && read_buffer.get_unread()[0] == "d".as_bytes()[0] {
                info!("message is DHT info, ignoring");
                continue;
            }

            let Ok((header, remaining)) = Header::read_from(read_buffer, read) else {
                warn!("UTP error: packet smaller than 20 bytes");
                continue;
            };
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
                torrents,
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
                        utp_socket,
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
            self.disconnect_peer_utp(addr, id, error, torrents);
        }
        Ok(num_packets)
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
            handshake.info_hash,
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

    pub fn disconnect_peer_utp(
        &mut self,
        addr: SocketAddr,
        id: u16,
        error: UpdateError,
        torrents: &mut HashMap<Sha1Hash, TorrentData>,
    ) -> Option<SocketAddr> {
        info!("Removing peer {}, {}: {} while updating", addr, id, error);
        if let Some(info_hash) = self.addr_to_info_hash.remove(&(addr, id)) {
            if let Some(torrent_data) = torrents.get_mut(&info_hash) {
                torrent_data.peers_data.disconnected_utp(addr, id);
            }
        }
        if let Some(_connection) = self.utp_connections.remove(&(addr, id)) {
            return Some(addr);
        }
        None
    }

    pub fn connect_to(
        &mut self,
        torrent: &mut TorrentData,
        peer_info: PeerInfo,
        utp_socket: &UdpSocket,
    ) {
        let mut connection = OutgoingUtpConnection::new(
            UtpConnectionInfo::new(peer_info.addr),
            torrent.info_hash,
            self.peer_id,
        );
        self.utp_send_buffer.add_header(Type::StSyn);
        let connection_id = connection.socket.conn_id_recv;
        if let Err(error) = self.utp_send_buffer.send(
            &mut self.utp_block_sender,
            utp_socket,
            &mut connection.socket,
        ) {
            warn!("Failed to connect to peer: {:?}", error);
            return;
        }
        torrent
            .peers_data
            .connected_utp(connection_id, peer_info.clone());
        self.utp_connections.insert(
            (peer_info.addr, connection_id),
            UtpConnection::Outgoing(connection),
        );
    }

    pub fn send_block(
        &mut self,
        addr: SocketAddr,
        conn_id: u16,
        block: Block,
        utp_socket: &UdpSocket,
    ) {
        let Some(UtpConnection::Established(conn)) = self.utp_connections.get_mut(&(addr, conn_id)) else {
            return
        };
        let utp_header = conn.get_utp_header();
        match self
            .utp_block_sender
            .send_block(utp_header, utp_socket, 1000, block, addr)
        {
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
