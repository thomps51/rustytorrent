pub mod connection_manager;
pub mod controller;
pub mod disk_manager;

pub mod connection;
use std::{
    io::{Read, Write},
    sync::mpsc::Sender,
};

use bit_vec::BitVec;
pub use connection::*;

pub mod block_manager;
pub use block_manager::*;
use log::debug;
use mio::{event::Source, net::TcpStream};
use std::fmt::Debug;
use write_to::{Name, ReadFrom};

use crate::{
    common::Sha1Hash,
    io::ReadBuffer,
    messages::{protocol_message::HasId, *},
};

use self::disk_manager::{ConnectionIdentifier, DiskRequest};

pub mod piece_info;

pub mod piece_assigner;

pub mod block_cache;

// pub mod initiating_connection;
pub mod peers_data;
pub mod tcp;
pub mod tracker_manager;
pub mod utp;

// pub trait NetworkSourceType: Source + Send + Write + Read + Debug {
pub trait NetworkSourceType: Source + Write + Read + Debug {
    fn peer_addr(&self) -> std::io::Result<std::net::SocketAddr>;
}

impl NetworkSourceType for TcpStream {
    fn peer_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.peer_addr()
    }
}

pub type NetworkSource = Box<dyn NetworkSourceType>;

pub struct PeerData {
    pub id: ConnectionIdentifier,
    pub peer_choking: bool,
    pub peer_interested: bool,
    pub peer_has: BitVec,
    pub pending_peer_cancels: Vec<Cancel>,
    pub disk_requester: Sender<DiskRequest>,
    pub num_pieces: usize,
    pub info_hash: Sha1Hash,
}

impl PeerData {
    fn new(
        id: ConnectionIdentifier,
        disk_requester: Sender<DiskRequest>,
        info_hash: Sha1Hash,
        num_pieces: usize,
    ) -> Self {
        PeerData {
            id,
            peer_choking: false,
            peer_interested: true,
            peer_has: BitVec::from_elem(num_pieces, false),
            pending_peer_cancels: Vec::new(),
            disk_requester,
            num_pieces,
            info_hash,
        }
    }
}

// Read message from buffer and modify peer_data
fn dispatch_message<'a, F: FnOnce(&'a mut ReadBuffer) -> UpdateResult>(
    read_buffer: &'a mut ReadBuffer,
    length: usize,
    id: u8,
    peer_data: &mut PeerData,
    handle_block: F,
) -> UpdateResult {
    // Block is handed elsewhere since it differs between tcp/utp
    macro_rules! dispatch_message2 (
            ($($A:ident => [$msg:ident] $B:block),*) => (
                match id {
                    Block::ID => {
                        handle_block(read_buffer)
                    }
                    $($A::ID => {
                        debug!("Reading {} message", $A::NAME);
                        let ($msg, length) = $A::read_from(read_buffer, length)?;/* .context("Reading message")?; */
                        debug!("Received {:?}", $msg);
                        assert_eq!(length, 0); // Not necessarily true in UTP? Only seems to happen with Blocks?
                        $B;
                        Ok(UpdateSuccess::Success)
                    })*
                    _ => Err(UpdateError::UnknownMessage{id}),
                }
            );
        );
    dispatch_message2!(
        Choke => [_msg] {
            peer_data.peer_choking = true;
        },
        Unchoke => [_msg] {
            peer_data.peer_choking = false;
        },
        Interested => [_msg] {
            peer_data.peer_interested = true;
        },
        NotInterested => [_msg] {
            peer_data.peer_interested = false;
        },
        Have => [msg] {
            if msg.index as usize >= peer_data.peer_has.len() {
                return Err(UpdateError::IndexOutOfBounds);
            }
            peer_data.peer_has.set(msg.index as usize, true);
        },
        Bitfield => [msg] {
            peer_data.peer_has = msg.bitfield;
            // Bitvec needs to be truncated since it contains padding
            peer_data.peer_has.truncate(peer_data.num_pieces);
        },
        Request => [msg] {
            peer_data.disk_requester
                .send(DiskRequest::Request {info_hash:peer_data.info_hash,conn_id:peer_data.id, request: msg })
                .unwrap();
            },
        Cancel => [msg] {
            peer_data.pending_peer_cancels.push(msg);
        },
        Port => [_msg] {
            // Simple ack, DHT not implemented
        }
    )
}

pub trait Connection {
    fn set_peer_choking(&mut self, value: bool);
    fn set_peer_interested(&mut self, value: bool);
    fn set_peer_has(&mut self, have: Have) -> std::io::Result<()>;
    fn set_peer_has_bitfield(&mut self, bitfield: Bitfield);
    fn peer_request(&mut self, request: Request) -> std::io::Result<()>;
    fn peer_cancel(&mut self, cancel: Cancel) -> std::io::Result<()>;
}
