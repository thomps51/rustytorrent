use std::io::prelude::*;

use bit_vec::BitVec;
use log::{debug, info};
use mio::net::TcpStream;
use mio::Poll;

use super::block_manager::BlockManager;
use super::{ConnectionBase, UpdateError, UpdateResult, UpdateSuccess};
use crate::client::PieceStore;
use crate::common::SharedPieceAssigner;
use crate::common::SharedPieceStore;
use crate::io::ReadBuffer;
use crate::messages::*;

pub struct EstablishedConnection {
    am_choking: bool,
    am_interested: bool,
    pub id: usize,
    pub peer_choking: bool,
    pub peer_interested: bool,
    pub peer_has: BitVec,
    stream: TcpStream,
    last_keep_alive: std::time::Instant,
    block_manager: BlockManager,
    pub pending_peer_requests: Vec<Request>,
    pub pending_peer_cancels: Vec<Cancel>,
    next_message_length: Option<usize>,
    read_buffer: ReadBuffer,
    send_buffer: Vec<u8>,
    pub num_pieces: usize,
    state: State,
    pub downloaded: usize,
}

pub enum State {
    ConnectedNormal,
    ConnectedEndgame,
    Seeding,
}

fn read_byte_from<T: Read>(stream: &mut T) -> Result<u8, std::io::Error> {
    let mut buffer = [0; 1];
    stream.read_exact(&mut buffer)?;
    Ok(buffer[0])
}

impl EstablishedConnection {
    pub fn new(
        id: usize,
        stream: TcpStream,
        num_pieces: usize,
        piece_assigner: SharedPieceAssigner,
        piece_store: SharedPieceStore,
    ) -> Self {
        Self {
            am_choking: true,
            am_interested: false,
            id,
            peer_choking: true,
            peer_interested: false,
            peer_has: BitVec::from_elem(num_pieces, false),
            stream,
            last_keep_alive: std::time::Instant::now(),
            block_manager: BlockManager::new(piece_assigner, piece_store),
            pending_peer_requests: Vec::new(),
            pending_peer_cancels: Vec::new(),
            next_message_length: None,
            read_buffer: ReadBuffer::new(1 << 20), // 1 MiB
            send_buffer: Vec::new(),
            num_pieces,
            state: State::ConnectedNormal,
            downloaded: 0,
        }
    }

    fn read(&mut self) -> UpdateResult {
        const LENGTH_BYTE_SIZE: usize = 4;
        let length = if let Some(v) = self.next_message_length {
            v
        } else {
            if !self
                .read_buffer
                .read_at_least_from(LENGTH_BYTE_SIZE, &mut self.stream)?
            {
                return Ok(UpdateSuccess::NoUpdate);
            }
            let value = read_as_be::<u32, _, _>(&mut self.read_buffer).unwrap();
            value
        };
        self.next_message_length = Some(length);
        if length == 0 {
            let msg = KeepAlive {};
            self.next_message_length = None;
            return msg.update(self);
        }
        if !self
            .read_buffer
            .read_at_least_from(length, &mut self.stream)?
        {
            return Ok(UpdateSuccess::NoUpdate);
        }
        let total_read = LENGTH_BYTE_SIZE + length;
        let id = read_byte_from(&mut self.read_buffer)? as i8;
        macro_rules! dispatch_message ( // This is really neat!
            ($($A:ident),*) => (
                match id {
                    Block::ID => {
                        Block::read_and_update(&mut self.read_buffer, &mut self.block_manager, length)?;
                        Ok(UpdateSuccess::Success)
                    },
                    $($A::ID => {
                        let msg = $A::read_from(&mut self.read_buffer, length)?;
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
        self.downloaded += total_read;
        match retval {
            Ok(UpdateSuccess::Success) => Ok(UpdateSuccess::Transferred {
                downloaded: total_read,
                uploaded: 0,
            }),
            _ => retval,
        }
    }

    // Read until EWOULDBLOCK or error occurs
    fn read_all(&mut self) -> UpdateResult {
        let mut total_downloaded = 0;
        let mut total_uploaded = 0;
        loop {
            let retval = self.read()?;
            match retval {
                UpdateSuccess::NoUpdate => {
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
    pub fn send<T: Message>(&mut self, message: &T) -> Result<bool, std::io::Error> {
        let mut result = false;
        if self.send_buffer.len() == 0 {
            message.write_to(&mut self.send_buffer).unwrap();
            result = true;
        }
        match self.stream.write(&self.send_buffer) {
            Ok(sent) => {
                if sent == self.send_buffer.len() {
                    // debug!("Wrote all buffer");
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

    pub fn send_to<T: Message, W: Write>(
        &self,
        message: &T,
        writer: &mut W,
        mut send_buffer: &mut Vec<u8>,
    ) -> Result<bool, std::io::Error> {
        let mut result = false;
        if send_buffer.len() == 0 {
            message.write_to(&mut send_buffer).unwrap();
            result = true;
        }
        match writer.write(&self.send_buffer) {
            Ok(sent) => {
                if sent == self.send_buffer.len() {
                    send_buffer.clear();
                } else {
                    send_buffer.drain(0..sent);
                }
            }
            Err(error) => {
                if error.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(error);
                }
            }
        }
        Ok(result)
    }

    fn send_block_requests(&mut self) -> UpdateResult {
        if !self.peer_choking {
            info!("Peer is not choking");
            let sent = self.block_manager.send_block_requests(
                &mut self.stream,
                &self.peer_has,
                self.id,
            )?;
            if sent == 0 {
                info!("No block requests sent");
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

    fn deregister(&mut self, poll: &mut Poll) {
        poll.registry().deregister(&mut self.stream).unwrap();
    }

    fn update(&mut self) -> UpdateResult {
        match self.state {
            State::Seeding => {
                let read_result = self.read_all()?;
                let mut to_send: Vec<Request> = self.pending_peer_requests.drain(..).collect();
                let mut to_send_drain = to_send.drain(..);
                loop {
                    if let Some(request) = to_send_drain.next() {
                        debug!("Processing request: {:?}", request);
                        let data = if let Some(data) = self
                            .block_manager
                            .piece_store
                            .borrow()
                            .get_block(request.piece_index(), request.offset())
                        {
                            debug!("Got {} bytes for request", data.len());
                            data
                        } else {
                            info!("failed to get requested piece: {:?}", request);
                            continue;
                        };
                        let msg = Block {
                            index: request.piece_index(),
                            begin: request.offset(),
                            block: data,
                        };
                        if !self.send(&msg)? {
                            self.pending_peer_requests = to_send_drain.collect();
                            info!("Got pushback, stopping sends");
                            break;
                        }
                    } else {
                        break;
                    }
                }
                Ok(read_result)
            }
            State::ConnectedNormal | State::ConnectedEndgame => {
                let read_result = self.read_all()?;
                // Cancel requested Requests (TODO)
                if !self.pending_peer_requests.is_empty() {
                    let mut to_send: Vec<Request> = self.pending_peer_requests.drain(..).collect();
                    let mut to_send_drain = to_send.drain(..);
                    loop {
                        if let Some(request) = to_send_drain.next() {
                            debug!("Processing request: {:?}", request);
                            let data = if let Some(data) = self
                                .block_manager
                                .piece_store
                                .borrow()
                                .get_block(request.piece_index(), request.offset())
                            {
                                debug!("Got {} bytes for request", data.len());
                                data
                            } else {
                                info!("failed to get requested piece: {:?}", request);
                                continue;
                            };
                            let msg = Block {
                                index: request.piece_index(),
                                begin: request.offset(),
                                block: data,
                            };
                            if !self.send(&msg)? {
                                self.pending_peer_requests = to_send_drain.collect();
                                info!("Got pushback, stopping sends");
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                if self.block_manager.piece_assigner.borrow().is_endgame() {
                    self.state = State::ConnectedEndgame;
                    return Ok(read_result);
                }
                if self.block_manager.is_done() {
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
