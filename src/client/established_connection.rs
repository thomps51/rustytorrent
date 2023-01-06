use std::io::prelude::*;
use std::sync::mpsc::Sender;

use bit_vec::BitVec;
use log::{debug, info};
use mio::Token;

use super::block_manager::BlockManager;
use super::disk_manager::{ConnectionIdentifier, DiskRequest};
use super::{ConnectionBase, NetworkSource, UpdateError, UpdateResult, UpdateSuccess};
use crate::common::{Sha1Hash, SharedBlockCache, SharedCount, SharedPieceAssigner};
use crate::io::ReadBuffer;
use crate::messages::protocol_message::HasId;
use crate::messages::ProtocolMessage;
use crate::messages::*;
use write_to::ReadFrom;

pub struct EstablishedConnection {
    pub info_hash: Sha1Hash,
    pub am_choking: bool,
    pub am_interested: bool,
    pub id: usize,
    pub peer_choking: bool,
    pub peer_interested: bool,
    pub peer_has: BitVec,
    stream: NetworkSource,
    last_keep_alive: std::time::Instant,
    block_manager: BlockManager,
    pub pending_peer_cancels: Vec<Cancel>,
    next_message_length: Option<usize>,
    incomplete_message_buffer: Vec<u8>,
    send_buffer: Vec<u8>,
    pub num_pieces: usize,
    state: State,
    pub downloaded: SharedCount,
    pub uploaded: SharedCount,
    disk_requester: Sender<DiskRequest>,
}

pub enum State {
    ConnectedNormal,
    ConnectedEndgame,
    Seeding,
}

impl EstablishedConnection {
    pub fn new(
        id: usize,
        info_hash: Sha1Hash,
        stream: NetworkSource,
        num_pieces: usize,
        piece_assigner: SharedPieceAssigner,
        disk_requester: Sender<DiskRequest>,
        block_cache: SharedBlockCache,
        downloaded: SharedCount,
        uploaded: SharedCount,
    ) -> Self {
        Self {
            info_hash,
            am_choking: true,
            am_interested: false,
            id,
            peer_choking: true,
            peer_interested: false,
            peer_has: BitVec::from_elem(num_pieces, false),
            stream,
            last_keep_alive: std::time::Instant::now(),
            block_manager: BlockManager::new(piece_assigner, block_cache),
            pending_peer_cancels: Vec::new(),
            next_message_length: None,
            incomplete_message_buffer: Vec::new(),
            send_buffer: Vec::new(),
            num_pieces,
            state: State::ConnectedNormal,
            downloaded,
            uploaded,
            disk_requester,
        }
    }

    fn read(&mut self, read_buffer: &mut ReadBuffer) -> UpdateResult {
        const LENGTH_BYTE_SIZE: usize = 4;
        if !self.incomplete_message_buffer.is_empty() {
            read_buffer
                .read_from(&mut self.incomplete_message_buffer.as_slice())
                .unwrap();
            self.incomplete_message_buffer.clear();
        }
        let length = if let Some(v) = self.next_message_length {
            v
        } else {
            if !read_buffer.read_at_least_from(LENGTH_BYTE_SIZE, &mut self.stream)? {
                return Ok(UpdateSuccess::NoUpdate);
            }
            let (value, _) = u32::read_from(read_buffer, LENGTH_BYTE_SIZE).unwrap();
            value as usize
        };
        self.next_message_length = Some(length);
        if length == 0 {
            self.next_message_length = None;
            self.received_keep_alive();
            return Ok(UpdateSuccess::Success);
        }
        if !read_buffer.read_at_least_from(length, &mut self.stream)? {
            return Ok(UpdateSuccess::NoUpdate);
        }
        let total_read = LENGTH_BYTE_SIZE + length;
        let id = read_byte(read_buffer)?;
        let length = length - 1; // subtract ID byte
        macro_rules! dispatch_message (
            ($($A:ident),*) => (
                match id {
                    Block::ID => {
                        Block::read_and_update(read_buffer, &mut self.block_manager, length)
                    },
                    $($A::ID => {
                        let (msg, length) = $A::read_from(read_buffer, length)?;
                        assert_eq!(length, 0);
                        msg.update(self)
                    })*
                    _ => Err(UpdateError::UnknownMessage{id}),
                }
            );
        );
        let retval = dispatch_message!(
            Choke,
            Unchoke,
            Interested,
            NotInterested,
            Have,
            Bitfield,
            Request,
            Cancel,
            Port
        );
        self.next_message_length = None;
        *self.downloaded.borrow_mut() += total_read;
        match retval {
            Ok(UpdateSuccess::Success) => Ok(UpdateSuccess::Transferred {
                downloaded: total_read,
                uploaded: 0,
            }),
            _ => retval,
        }
    }

    // Read until EWOULDBLOCK or error occurs
    fn read_all(&mut self, read_buffer: &mut ReadBuffer) -> UpdateResult {
        let mut total_downloaded = 0;
        let mut total_uploaded = 0;
        loop {
            let retval = self.read(read_buffer)?;
            match retval {
                UpdateSuccess::NoUpdate => {
                    self.incomplete_message_buffer
                        .write_all(read_buffer.get_unread())
                        .unwrap();
                    read_buffer.clear();
                    break;
                }
                UpdateSuccess::Transferred {
                    downloaded,
                    uploaded,
                } => {
                    total_downloaded += downloaded;
                    total_uploaded += uploaded;
                    continue;
                }
                UpdateSuccess::Success => continue,
            }
        }
        if total_downloaded != 0 {
            Ok(UpdateSuccess::Transferred {
                downloaded: total_downloaded,
                uploaded: total_uploaded,
            })
        } else {
            Ok(UpdateSuccess::NoUpdate)
        }
    }

    pub fn received_keep_alive(&mut self) {
        self.last_keep_alive = std::time::Instant::now();
    }

    // True: message was used
    // False: message was dropped because of TCP pushback
    // If we get pushback, we keep the remaining piece of the message that failed to send
    // to send later so we don't send incomplete messages. If we do get pushback, this
    // logic will always drop at least 1 message since we try to push out the remaining
    // buffer at the expense of the next message we are trying to send.
    pub fn send<T: ProtocolMessage>(&mut self, message: &T) -> Result<bool, std::io::Error> {
        let mut result = false;
        if self.send_buffer.is_empty() {
            debug!(
                "Writing message {} with id {} and length {}, be_length: {:?}, from_be: {}, from_le: {}",
                T::NAME,
                T::ID,
                message.length(),
                message.length_be_bytes(),
                u32::from_be_bytes(message.length_be_bytes()),
                u32::from_le_bytes(message.length_be_bytes()),
            );
            message.write(&mut self.send_buffer).unwrap();
            result = true;
        }
        match self.stream.write(&self.send_buffer) {
            Ok(sent) => {
                if sent == self.send_buffer.len() {
                    self.send_buffer.clear();
                } else {
                    debug!("Wrote partial buffer");
                    self.send_buffer.drain(0..sent);
                }
            }
            Err(error) => {
                if error.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(error);
                }
                debug!("EWOULDBLOCK while sending on Connection {}", self.id);
            }
        }
        Ok(result)
    }

    pub fn send_disk_request(&self, request: Request) {
        self.disk_requester
            .send(DiskRequest::Request {
                info_hash: self.info_hash,
                conn_id: ConnectionIdentifier::TcpToken(Token(self.id)),
                request,
            })
            .unwrap();
    }

    fn send_block_requests(&mut self) -> UpdateResult {
        if !self.peer_choking {
            debug!("Peer is not choking");
            self.send_buffer.clear();
            let sent = self.block_manager.send_block_requests(
                &mut self.send_buffer,
                &self.peer_has,
                self.id,
            )?;
            self.stream.write_all(&self.send_buffer)?;
            self.send_buffer.clear();
            if sent == 0 {
                debug!("No block requests sent");
                return Ok(UpdateSuccess::NoUpdate);
            }
            return Ok(UpdateSuccess::Success);
        } else {
            info!("Peer is choking");
        }
        Ok(UpdateSuccess::NoUpdate)
    }
}

impl ConnectionBase for EstablishedConnection {
    type UpdateSuccessType = UpdateSuccess;

    fn into_network_source(self) -> NetworkSource {
        self.stream
    }

    fn update(&mut self, read_buffer: &mut ReadBuffer) -> UpdateResult {
        let read_result = match self.read_all(read_buffer) {
            Ok(read_result) => read_result,
            Err(error) => {
                read_buffer.clear();
                return Err(error);
            }
        };

        match self.state {
            State::Seeding => Ok(read_result),
            State::ConnectedNormal | State::ConnectedEndgame => {
                // Cancel requested Requests (TODO)
                // if self.block_manager.piece_assigner.borrow().is_endgame() {
                //     debug!("Transition to endgame");
                //     self.state = State::ConnectedEndgame;
                //     return Ok(read_result);
                // }
                if self.block_manager.block_cache.borrow().done() {
                    self.state = State::Seeding;
                    return Ok(read_result);
                }
                debug!("Connection {} sending block requests", self.id);
                let request_result = self.send_block_requests()?;
                match (&read_result, request_result) {
                    (UpdateSuccess::NoUpdate, UpdateSuccess::NoUpdate) => {
                        Ok(UpdateSuccess::NoUpdate)
                    }
                    (UpdateSuccess::NoUpdate, UpdateSuccess::Success) => Ok(UpdateSuccess::Success),
                    (_, _) => Ok(read_result),
                }
            }
        }
    }
}
