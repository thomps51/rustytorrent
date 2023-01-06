use std::collections::BTreeMap;
use std::io::{Write, Read, self};
use std::sync::mpsc::Sender;

use crate::client::disk_manager::{ConnectionIdentifier, DiskRequest};
use crate::client::{BlockManager, UpdateError, UpdateResult};
use crate::common::BLOCK_LENGTH;
use crate::messages::protocol_message::HasId;
use crate::{
    client::UpdateSuccess,
    common::{Sha1Hash, SharedBlockCache, SharedCount, SharedPieceAssigner},
    io::ReadBuffer,
    messages::{
        read_byte, Bitfield, Block, Cancel, Choke, Have, Interested, NotInterested, Port,
        ProtocolMessage, Request, Unchoke,
    },
};
use bit_vec::BitVec;
use log::debug;
use log::info;
use write_to::ReadFrom;

use super::{Header, Type, UtpSocket};

pub struct EstablishedUtpConnection {
    pub info_hash: Sha1Hash,
    pub am_choking: bool,
    pub am_interested: bool,
    pub id: usize,
    peer_choking: bool,
    peer_interested: bool,
    peer_has: BitVec,
    stream: UtpSocket,
    last_keep_alive: std::time::Instant,
    block_manager: BlockManager,
    pending_peer_cancels: Vec<Cancel>,
    num_pieces: usize,
    state: State,
    pub downloaded: SharedCount,
    pub uploaded: SharedCount,
    disk_requester: Sender<DiskRequest>,
    // For now, assume we only have one block in flight at a time.  In reality, messages
    // can arrive out of order, so we have to worry about the case where we received a
    // message that is actually for the next block, or is a different Bittorrent message
    // altogether
    current_block: BlockInFlight,
    // Whn we receive a seq_nr out of order, we
    next_sequence_number: u16,
    out_of_order: BTreeMap<u16, Vec<u8>>, // seq_nr to data
}

pub enum State {
    ConnectedNormal,
    ConnectedEndgame,
    Seeding,
}

struct BlockInFlight {
    index: usize,
    begin: usize,
    block: Vec<u8>,
}

impl BlockInFlight {
    fn new() -> Self {
        Self { index: 0, begin: 0, block: Vec::with_capacity(BLOCK_LENGTH) }
    }

    fn add(&mut self, reader: &mut impl Read, length: usize) -> bool {
        let remaining = BLOCK_LENGTH - self.block.len();
        let length = std::cmp::min(remaining, length);
        let current_block_length = self.block.len();
        // TODO: avoid overhead of zeroing vec since it doesn't matter
        let end = current_block_length + length;
        self.block.resize(end, 0);
        let target = &mut self.block.as_mut_slice()[current_block_length..end];
        reader.read_exact(target).unwrap();
        if self.block.len() == BLOCK_LENGTH {
            return true;
        }
        false
    }

    fn begin(&mut self, reader: &mut impl Read, length: usize) -> io::Result<bool> {
            let (index, length) = u32::read_from(reader, length)?;
            let (begin, length) = u32::read_from(reader, length)?;
            self.set_info(index as _, begin as _);
            Ok(self.add(reader, length))
    }

    fn active(&self) -> bool {
        !self.block.is_empty()
    }

    fn set_info(&mut self, index: usize, begin: usize) {
        self.index = index;
        self.begin = begin;
        self.reset();
    }

    fn reset(&mut self) {
        // We do this so we can reuse this
        self.block.clear();
    }
}

impl EstablishedUtpConnection {
    pub fn new(
        id: usize,
        info_hash: Sha1Hash,
        stream: UtpSocket,
        num_pieces: usize,
        piece_assigner: SharedPieceAssigner,
        disk_requester: Sender<DiskRequest>,
        block_cache: SharedBlockCache,
        downloaded: SharedCount,
        uploaded: SharedCount,
    ) -> Self {
        let next_sequence_number = stream.seq_nr;
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
            num_pieces,
            state: State::ConnectedNormal,
            downloaded,
            uploaded,
            disk_requester,
            current_block: BlockInFlight::new(),
            next_sequence_number,
            out_of_order: BTreeMap::new(),
        }
    }

    // Read until EWOULDBLOCK or error occurs
    fn read_all(&mut self, read_buffer: &mut ReadBuffer, header: &Header) -> UpdateResult {
        let mut total_downloaded = 0;
        let mut total_uploaded = 0;

        if header.seq_nr != self.next_sequence_number {
            debug!(
                "Received out of order utp packet, expected {} got {}",
                self.next_sequence_number, header.seq_nr
            );
            self.out_of_order
                .insert(header.seq_nr, read_buffer.get_unread().to_owned());
            // TODO: do something to indicate we have these?
            // Maybe we want to try to process these after we get an ACK as well?
            return Ok(UpdateSuccess::NoUpdate);
        }
        let (next_sequence_number, _) = self.next_sequence_number.overflowing_add(1);
        self.next_sequence_number = next_sequence_number;
        loop {
            let retval = self.read(read_buffer, header)?;
            if read_buffer.unread() == 0 {
                loop {
                    // TODO: Write tests that actually test this logic
                    if let Some((ooo_seq_nr, ooo_data)) = self.out_of_order.first_key_value() 
                        && *ooo_seq_nr == self.next_sequence_number
                    {
                        read_buffer.write_all(ooo_data)?;
                        self.next_sequence_number += 1;
                        self.out_of_order.pop_first();
                    } else {
                        break;
                    }
                }
            } else {
                break;
            }
            match retval {
                UpdateSuccess::NoUpdate => {
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
        // Ok(UpdateSuccess::NoUpdate)
        //     UpdateSuccess::NoUpdate.into()
        // };
        if total_downloaded != 0 {
            Ok(UpdateSuccess::Transferred {
                downloaded: total_downloaded,
                uploaded: total_uploaded,
            })
        } else {
            Ok(UpdateSuccess::NoUpdate)
        }
    }

    fn read(&mut self, read_buffer: &mut ReadBuffer, _header: &Header) -> UpdateResult {
        // If we are in the middle of a block, we will be getting packets with an ST_DATA header and
        // a chunk of data, with no length prefix.

        // I need to know if the header was ST_DATA here, or make it so only ST_DATA packets get here
        if self.current_block.active() {
            let length = read_buffer.unread();
            if self.current_block.add(read_buffer, length) {
                    Block::read_and_update_utp(&mut &*self.current_block.block,  &mut self.block_manager, self.current_block.block.len())?;
                    self.current_block.reset();
            } else {
                return Ok(UpdateSuccess::Transferred {
                                    downloaded: length,
                                    uploaded: 0,
                });
            }
            return Ok(UpdateSuccess::Transferred {
                downloaded: length,
                uploaded: 0,
            });
        }

        const LENGTH_BYTE_SIZE: usize = 4;
        let (length, _) = u32::read_from(read_buffer, LENGTH_BYTE_SIZE)?;
        let length = length as usize;
        if length == 0 {
            self.received_keep_alive();
            return Ok(UpdateSuccess::Success);
        }
        let total_read = LENGTH_BYTE_SIZE + length;
        let id = read_byte(read_buffer)?;
        let length = length - 1; // subtract ID byte
        let mut update_block = || -> UpdateResult {
            if self.current_block.begin(read_buffer, length)? {
                Block::read_and_update_utp(&mut &*self.current_block.block,  &mut self.block_manager, self.current_block.block.len())?;
                self.current_block.reset();
            }
            Ok(UpdateSuccess::Success)
        };
        macro_rules! dispatch_message2 (
            ($($A:ident => [$msg:ident] $B:block),*) => (
                match id {
                    Block::ID => {
                        update_block()
                    },
                    $($A::ID => {
                        let ($msg, length) = $A::read_from(read_buffer, length)?;
                        assert_eq!(length, 0);
                        $B;
                        Ok(UpdateSuccess::Success)
                    })*
                    _ => Err(UpdateError::UnknownMessage{id}),
                }
            );
        );
        let retval = dispatch_message2!(
            Choke => [_msg] {
                self.peer_choking = true;
            },
            Unchoke => [_msg] {
                self.peer_choking = false;
            },
            Interested => [_msg] {
                self.peer_interested = true;
            },
            NotInterested => [_msg] {
                self.peer_interested = false;
            },
            Have => [msg] {
                if msg.index as usize >= self.peer_has.len() {
                    return Err(UpdateError::IndexOutOfBounds);
                }
                self.peer_has.set(msg.index as usize, true);
            },
            Bitfield => [msg] {
                self.peer_has = msg.bitfield;
                self.peer_has.truncate(self.num_pieces);
            },
            Request => [msg] {
                self.send_disk_request(msg);
            },
            Cancel => [msg] {
                self.pending_peer_cancels.push(msg);
            },
            Port => [_msg] {
                // Simple ack, DHT not implemented
            }
        );
        *self.downloaded.borrow_mut() += total_read;
        match retval {
            Ok(UpdateSuccess::Success) => Ok(UpdateSuccess::Transferred {
                downloaded: total_read,
                uploaded: 0,
            }),
            _ => retval,
        }
    }

    pub fn received_keep_alive(&mut self) {
        self.last_keep_alive = std::time::Instant::now();
    }

    pub fn send<T: ProtocolMessage>(&mut self, message: &T) -> std::io::Result<()> {
        self.stream.write(message)?;
        Ok(())
    }

    pub fn send_disk_request(&self, request: Request) {
        self.disk_requester
            .send(DiskRequest::Request {
                info_hash: self.info_hash,
                conn_id: ConnectionIdentifier::UtpId(self.stream.addr()),
                request,
            })
            .unwrap();
    }

    fn send_block_requests(&mut self) -> UpdateResult {
        if !self.peer_choking {
            debug!("Peer is not choking");
            let sent = self.block_manager.send_block_requests_utp(
                &mut self.stream,
                &self.peer_has,
                self.id,
            )?;
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

impl EstablishedUtpConnection {
    pub fn update(&mut self, read_buffer: &mut ReadBuffer, header: &Header) -> UpdateResult {
        // If header is ST_STATE, we don't need to process it.
        if header.get_type() == Type::StState {
            return Ok(UpdateSuccess::NoUpdate);
        }
        self.stream.process_header(header);
        // TODO: maybe ACK only completed blocks?
        self.stream.send_header(Type::StState)?;
        let read_result = match self.read_all(read_buffer, header) {
            Ok(read_result) => read_result,
            Err(error) => {
                log::debug!("EstablishedUtpConnection update read error: {:?}", error);
                read_buffer.clear();
                UpdateSuccess::NoUpdate
                // if header.get_type() == Type::StState {
                //     // TODO handle this better, find out what to do with bits in buffer
                //     UpdateSuccess::NoUpdate
                // } else {
                //     UpdateSuccess::NoUpdate
                //     // return Err(error);
                // }
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
