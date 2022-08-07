use log::debug;
use log::info;
use std::{collections::HashMap, io::Read, sync::mpsc::Sender};

use bit_vec::BitVec;

use crate::common::BLOCK_LENGTH;
use crate::messages::Cancel;
use crate::{
    common::{is_valid_piece, Sha1Hash},
    messages::{BlockReader, Request},
};

use super::{disk_manager::DiskRequest, piece_info::PieceInfo};

// This needs to be shared among connections of a torrent
pub struct BlockCache {
    info_hash: Sha1Hash,
    piece_info: PieceInfo,
    write_cache: HashMap<usize, PieceInFlight>, // TODO: this can get arbitrarily large if we get many pieces in flight (unlikely at least)
    have_maybe_unwritten_to_disk: BitVec,
    have_count: usize,
    sender: Sender<DiskRequest>,
    pieces: Vec<Sha1Hash>,
}

impl BlockCache {
    pub fn new(
        info_hash: Sha1Hash,
        piece_info: PieceInfo,
        have: BitVec,
        sender: Sender<DiskRequest>,
        pieces: Vec<Sha1Hash>,
    ) -> Self {
        let have_count = have.iter().filter(|x| *x).count();
        Self {
            info_hash,
            piece_info,
            write_cache: HashMap::new(),
            have_maybe_unwritten_to_disk: have,
            have_count,
            sender,
            pieces,
        }
    }

    pub fn endgame_get_unreceived_blocks(&self) -> Vec<Request> {
        let mut result = Vec::new();
        info!(
            "Endgame unreceived: Pieces in flight: {}",
            self.write_cache.len()
        );
        for piece_in_flight in self.write_cache.values() {
            for (block_index, have) in piece_in_flight.have.iter().enumerate() {
                if !have {
                    result.push(Request::new(
                        block_index,
                        piece_in_flight.index,
                        self.piece_info,
                    ));
                }
            }
        }
        for (piece_index, value) in self.have_maybe_unwritten_to_disk.iter().enumerate() {
            if !value && !self.write_cache.contains_key(&piece_index) {
                for block_index in 0..self.piece_info.get_num_blocks(piece_index) {
                    result.push(Request::new(block_index, piece_index, self.piece_info));
                }
            }
        }
        result
    }

    pub fn done(&self) -> bool {
        self.have_count == self.pieces.len()
    }

    pub fn have_maybe_unwritten_to_disk(&self) -> BitVec {
        self.have_maybe_unwritten_to_disk.clone()
    }

    pub fn percent_done(&self) -> f64 {
        return self.have_count as f64 * 100.0 / self.piece_info.total_pieces as f64;
    }

    pub fn write_block<T: Read>(&mut self, block: BlockReader<T>) -> Result<(), std::io::Error> {
        let piece_index = block.piece_index();
        if self.have_maybe_unwritten_to_disk[piece_index] {
            debug!("Got block for already received piece");
            return Ok(());
        }
        let piece_info = self.piece_info;
        let piece = self
            .write_cache
            .entry(block.piece_index())
            .or_insert_with(|| PieceInFlight::new(block.piece_index(), piece_info));
        if let Ok(Some(CompletedPiece { index, piece })) = piece.add_block(block) {
            if is_valid_piece(&piece, piece_index, &self.pieces) {
                self.have_maybe_unwritten_to_disk.set(piece_index, true);
                self.have_count += 1;
                self.sender
                    .send(DiskRequest::WritePiece {
                        info_hash: self.info_hash,
                        index,
                        data: piece,
                    })
                    .unwrap();
            } else {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            self.write_cache.remove(&piece_index);
        }
        Ok(())
    }

    fn get_block_have(&self, index: usize) -> BitVec {
        let num_blocks = self.piece_info.get_num_blocks(index);
        if self.have_maybe_unwritten_to_disk[index] {
            return BitVec::from_elem(num_blocks, true);
        }
        if let Some(value) = self.write_cache.get(&index) {
            value.have.clone()
        } else {
            BitVec::from_elem(num_blocks, false)
        }
    }

    pub fn reconcile(
        &self,
        sent_block_requests: &mut HashMap<usize, BitVec>,
    ) -> (usize, Vec<Cancel>) {
        // each one that is removed needs to be cancelled.
        let mut to_remove = Vec::new();
        let mut to_cancel = Vec::new();
        for (piece_index, sent_blocks) in sent_block_requests.iter_mut() {
            if self.have_maybe_unwritten_to_disk[*piece_index] {
                sent_blocks.set_all();
                sent_blocks.negate();
            } else {
                let mut have = self.get_block_have(*piece_index);
                have.and(&sent_blocks);
                for (block_index, value) in have.iter().enumerate() {
                    if value {
                        let begin = BLOCK_LENGTH * block_index;
                        to_cancel.push(Cancel {
                            index: *piece_index,
                            begin,
                            length: BLOCK_LENGTH,
                        });
                        sent_blocks.set(block_index, false);
                    }
                }
            }
            if sent_blocks.none() {
                to_remove.push(*piece_index);
            }
        }
        for index in to_remove {
            sent_block_requests.remove(&index);
        }
        let mut result = 0;
        for (_, sent_blocks) in sent_block_requests {
            sent_blocks.iter().for_each(|x| result += x as usize);
        }
        (result, to_cancel)
    }
}

#[derive(Debug)]
pub struct PieceInFlight {
    pub index: usize,
    piece: Vec<u8>,
    pub have: BitVec,
    piece_info: PieceInfo,
    blocks_received: usize,
}

impl PieceInFlight {
    pub fn new(index: usize, piece_info: PieceInfo) -> PieceInFlight {
        let length = piece_info.get_piece_length(index);
        let mut piece = Vec::new();
        piece.reserve(length);
        unsafe { piece.set_len(length) }
        let num_blocks = piece_info.get_num_blocks(index);
        let have = BitVec::from_elem(num_blocks, false);
        PieceInFlight {
            index,
            piece,
            have,
            piece_info,
            blocks_received: 0,
        }
    }

    pub fn add_block<T: Read>(
        &mut self,
        mut block: BlockReader<T>,
    ) -> Result<Option<CompletedPiece>, std::io::Error> {
        debug_assert!(block.piece_index() == self.index);
        debug_assert!(self.piece.len() != 0);
        if block.block_index() >= self.have.len() {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        if self.have[block.block_index()] {
            return Ok(None);
        }
        let block_length = self
            .piece_info
            .get_block_length(block.block_index(), self.index);
        if block_length != block.len() {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        self.blocks_received += 1;
        let end = block.begin() + block.len();
        block
            .read(&mut self.piece[block.begin()..end])
            .expect("Reading from read buffer");
        self.have.set(block.block_index(), true);
        if self.blocks_received == self.have.len() {
            debug!("Got piece {}", self.index);
            Ok(Some(CompletedPiece {
                index: self.index,
                piece: std::mem::replace(&mut self.piece, Vec::new()),
            }))
        } else {
            Ok(None)
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompletedPiece {
    pub index: usize,
    pub piece: Vec<u8>,
}
