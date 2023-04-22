use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::sync::mpsc::Sender;

use crate::client::disk_manager::{ConnectionIdentifier, DiskRequest};
use crate::client::{dispatch_message, BlockManager, PeerData, UpdateError, UpdateResult};
use crate::common::BLOCK_LENGTH;
use crate::{
    client::UpdateSuccess,
    common::{Sha1Hash, SharedBlockCache, SharedCount, SharedPieceAssigner},
    io::ReadBuffer,
    messages::{read_byte, Block},
};
use anyhow::Context;
use log::debug;
use log::info;
use write_to::ReadFrom;

use super::{Header, Type, UtpConnectionInfo, UtpSendBuffer};

pub struct EstablishedUtpConnection {
    pub info_hash: Sha1Hash,
    pub am_choking: bool,
    pub am_interested: bool,
    pub id: ConnectionIdentifier,
    peer_data: PeerData,
    pub(crate) stream: UtpConnectionInfo,
    last_keep_alive: std::time::Instant,
    block_manager: BlockManager,
    state: State,
    pub downloaded: SharedCount,
    pub uploaded: SharedCount,
    current_block: BlockInFlight,
    next_sequence_number: u16,
    out_of_order: BTreeMap<u16, Vec<u8>>, // seq_nr to data
    incomplete_message_buffer: Vec<u8>,
}

pub enum State {
    ConnectedNormal,
    ConnectedEndgame,
    Seeding,
}

struct BlockInFlight {
    index: Option<usize>,
    begin: Option<usize>,
    block: Vec<u8>,
    length: usize,
    active: bool,
}

impl BlockInFlight {
    fn new() -> Self {
        Self {
            index: None,
            begin: None,
            block: Vec::with_capacity(BLOCK_LENGTH),
            length: BLOCK_LENGTH,
            active: false,
        }
    }

    fn add(&mut self, reader: &mut impl Read, mut length: usize) -> bool {
        if self.index.is_none() {
            let (index, remaining) = u32::read_from(reader, length).unwrap();
            length = remaining;
            self.index = Some(index as _);
        }
        if self.begin.is_none() {
            let (begin, remaining) = u32::read_from(reader, length).unwrap();
            length = remaining;
            self.begin = Some(begin as _);
        }
        let remaining = self.length - self.block.len();
        let length = std::cmp::min(remaining, length);
        let current_block_length = self.block.len();
        // TODO: avoid overhead of zeroing vec since it doesn't matter
        let end = current_block_length + length;
        self.block.resize(end, 0);
        let target = &mut self.block.as_mut_slice()[current_block_length..end];
        debug!(
            "length: {}, end: {}, target_len: {}",
            length,
            end,
            target.len()
        );
        reader.read_exact(target).unwrap();
        if self.block.len() == self.length {
            return true;
        }
        false
    }

    fn begin(
        &mut self,
        reader: &mut impl Read,
        message_length: usize,
        first_packet_length: usize,
    ) -> io::Result<bool> {
        self.reset();
        self.active = true;
        self.length = message_length - 8;
        // index and begin won't necessarily be included in this packet. If they are not, they will be in the next one
        // If this function is called, then we know we have index and begin
        let (index, length) = u32::read_from(reader, first_packet_length).unwrap();
        self.index = Some(index as _);
        let (begin, length) = u32::read_from(reader, length).unwrap();
        self.begin = Some(begin as _);
        debug!("Setting BlockInFlight index: {}, begin: {}", index, begin);
        debug!("Remaining data length: {}", length);
        Ok(self.add(reader, length))
    }

    fn active(&self) -> bool {
        self.active
    }

    fn reset(&mut self) {
        self.block.clear();
        self.active = false;
        self.index = None;
        self.begin = None;
    }
}

impl EstablishedUtpConnection {
    pub fn new(
        id: ConnectionIdentifier,
        info_hash: Sha1Hash,
        stream: UtpConnectionInfo,
        num_pieces: usize,
        piece_assigner: SharedPieceAssigner,
        disk_requester: Sender<DiskRequest>,
        block_cache: SharedBlockCache,
        downloaded: SharedCount,
        uploaded: SharedCount,
    ) -> Self {
        let next_sequence_number = stream.ack_nr;
        Self {
            info_hash,
            am_choking: true,
            am_interested: false,
            id,
            peer_data: PeerData::new(id, disk_requester, info_hash, num_pieces),
            stream,
            last_keep_alive: std::time::Instant::now(),
            block_manager: BlockManager::new(piece_assigner, block_cache),
            state: State::ConnectedNormal,
            downloaded,
            uploaded,
            current_block: BlockInFlight::new(),
            next_sequence_number,
            out_of_order: BTreeMap::new(),
            incomplete_message_buffer: vec![],
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
            let retval = self.read(read_buffer, header).unwrap();
            debug!("Remaining read buffer size: {}", read_buffer.unread());
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
                if read_buffer.unread() == 0 {
                    break;
                }
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
        if !self.incomplete_message_buffer.is_empty() {
            read_buffer.prepend_unread(self.incomplete_message_buffer.as_slice());
            self.incomplete_message_buffer.clear();
        }
        let packet_data_length = read_buffer.unread();
        if packet_data_length == 0 {
            return Ok(UpdateSuccess::NoUpdate);
        }
        if self.current_block.active() {
            if self.current_block.add(read_buffer, packet_data_length) {
                Block::read_and_update_utp(
                    &mut &*self.current_block.block,
                    &mut self.block_manager,
                    self.current_block.index.unwrap(),
                    self.current_block.begin.unwrap(),
                    self.current_block.length,
                )?;
                self.current_block.reset();
            }
            return Ok(UpdateSuccess::Transferred {
                downloaded: packet_data_length,
                uploaded: 0,
            });
        }
        const LENGTH_BYTE_SIZE: usize = 4;
        if read_buffer.unread() < LENGTH_BYTE_SIZE + ID_BYTE_SIZE {
            debug!("Incomplete message, not enough for length and id");
            read_buffer.read_to_end(&mut self.incomplete_message_buffer)?;
            return Ok(UpdateSuccess::NoUpdate);
        }
        let (length, _) =
            u32::read_from(read_buffer, LENGTH_BYTE_SIZE).context("Reading message length")?;
        let length = length as usize;
        if length == 0 {
            self.received_keep_alive();
            return Ok(UpdateSuccess::Success);
        }
        let total_read = LENGTH_BYTE_SIZE + length;
        const ID_BYTE_SIZE: usize = 1;
        let id = read_byte(read_buffer).context("Reading message ID")?;
        let length = length - ID_BYTE_SIZE;
        let packet_data_length = packet_data_length - LENGTH_BYTE_SIZE - ID_BYTE_SIZE;
        let update_block = |read_buffer: &mut ReadBuffer| -> UpdateResult {
            debug!("update_block");
            if read_buffer.unread() < 8 {
                debug!("incompleete block");
                self.current_block.reset();
                self.current_block.active = true;
                self.current_block.length = length - 8;
                read_buffer.read_to_end(&mut self.incomplete_message_buffer)?;
                return Ok(UpdateSuccess::NoUpdate);
            }
            if self
                .current_block
                .begin(read_buffer, length, packet_data_length)
                .context("Beginning block")?
            {
                Block::read_and_update_utp(
                    &mut &*self.current_block.block,
                    &mut self.block_manager,
                    self.current_block.index.unwrap(),
                    self.current_block.begin.unwrap(),
                    self.current_block.length,
                )?;
                self.current_block.reset();
            }
            Ok(UpdateSuccess::Success)
        };
        let retval = dispatch_message(read_buffer, length, id, &mut self.peer_data, update_block);
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

    pub fn get_utp_header(&mut self) -> Header {
        self.stream.create_header(Type::StData)
    }

    pub fn add_seq_nr(&mut self, sent: usize) {
        let value = self.stream.seq_nr.wrapping_add(sent as _);
        self.stream.seq_nr = value;
    }

    fn send_block_requests(&mut self, send_buffer: &mut UtpSendBuffer) -> UpdateSuccess {
        if !self.peer_data.peer_choking {
            debug!("Peer is not choking");
            let sent = self
                .block_manager
                .send_block_requests(send_buffer, &self.peer_data.peer_has, self.id)
                .expect("write to send_buffer never fails");
            if sent == 0 {
                debug!("No block requests sent");
                return UpdateSuccess::NoUpdate;
            }
            return UpdateSuccess::Success;
        } else {
            info!("Peer is choking");
        }
        UpdateSuccess::NoUpdate
    }
}

impl EstablishedUtpConnection {
    pub fn update(
        &mut self,
        read_buffer: &mut ReadBuffer,
        header: &Header,
        send_buffer: &mut UtpSendBuffer,
    ) -> UpdateResult {
        if header.get_type() == Type::StState {
            // ACK
            return Ok(UpdateSuccess::NoUpdate);
        }
        // TODO: Handle ST_FIN specifically
        if header.get_type() != Type::StData {
            return Err(UpdateError::CommunicationError(
                std::io::ErrorKind::InvalidData.into(),
            ));
        }
        self.stream.process_header(header);
        // TODO: maybe ACK only completed blocks?
        send_buffer.add_header(Type::StState);
        let read_result = match self.read_all(read_buffer, header) {
            Ok(read_result) => read_result,
            Err(error) => {
                log::debug!("EstablishedUtpConnection update read error: {:?}", error);
                read_buffer.clear();
                UpdateSuccess::NoUpdate
            }
        };

        match self.state {
            State::Seeding => Ok(read_result),
            State::ConnectedNormal | State::ConnectedEndgame => {
                if self.block_manager.block_cache.borrow().done() {
                    self.state = State::Seeding;
                    return Ok(read_result);
                }
                debug!("Connection {:?} sending block requests", self.id);
                let request_result = self.send_block_requests(send_buffer);
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
