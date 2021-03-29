use std::collections::HashMap;
use std::io::prelude::*;

use bit_vec::BitVec;
use log::{debug, info};

use crate::messages::*;
use crate::SharedPieceAssigner;
use crate::SharedPieceStore;

// Stats for Vuze
// Value : Max Speed (MB/s)
// 10    : 2 MB/s
// 20    : 4 MB/s
// 50    : 12 MB/s
// 75    : 18 MB/s
// 100   : 18 MB/s
// 200   : 18 MB/s
//
// Stats for qBittorrent:
// 15    : 100 MiB/s
//
// In real swarm (some one in each test):
// 15    : 12 MiB/s
// 30    : 25 MiB/s
// 45    : 32 MiB/s
// 100   : 62 MiB/s
// 200   : 32 MiB/s
// I likely want a dynamic value based on if the peer completed it's requested pieces within a certain amount of time.
// Could also do it as a function of ping
const MAX_OPEN_REQUESTS_PER_PEER: usize = 10;

// Manages requests and receipts of blocks.
pub struct BlockManager {
    blocks_in_flight: usize,
    piece_assigner: SharedPieceAssigner,
    piece_store: SharedPieceStore,
    endgame_sent_blocks: HashMap<usize, BitVec>,
}

impl BlockManager {
    pub fn new(piece_assigner: SharedPieceAssigner, piece_store: SharedPieceStore) -> Self {
        BlockManager {
            blocks_in_flight: 0,
            piece_assigner,
            piece_store,
            endgame_sent_blocks: HashMap::new(),
        }
    }

    pub fn add_block_fn<F: FnOnce(&mut [u8]) -> Result<(), std::io::Error>>(
        &mut self,
        index: usize,
        begin: usize,
        length: usize,
        func: F,
    ) -> Result<(), std::io::Error> {
        self.blocks_in_flight -= 1;
        self.piece_store
            .borrow_mut()
            .write_block_fn(index, begin, length, func)?;
        Ok(())
    }

    pub fn send_block_requests<T: Write>(
        &mut self,
        stream: &mut T,
        peer_has: &BitVec,
        id: usize,
    ) -> Result<usize, std::io::Error> {
        let mut sent = 0;
        let mut piece_assigner = self.piece_assigner.borrow_mut();
        if let crate::piece_assigner::Mode::Endgame = piece_assigner.mode {
            let (blocks_in_flight, cancels) = self
                .piece_store
                .borrow()
                .endgame_reconcile(&mut self.endgame_sent_blocks);
            self.blocks_in_flight = blocks_in_flight;
            for cancel in cancels {
                cancel.write_to(stream)?;
            }
            /*
            info!(
                "Endgame blocks in flight for connection {}: {}, prev: {}",
                id, self.blocks_in_flight, prev
            );
            */
        }
        while self.blocks_in_flight < MAX_OPEN_REQUESTS_PER_PEER {
            if let Some((request, block_info)) = piece_assigner.get_block(
                &peer_has,
                || self.piece_store.borrow().endgame_get_unreceived_blocks(),
                id,
            ) {
                if piece_assigner.mode == crate::piece_assigner::Mode::Endgame {
                    self.endgame_sent_blocks
                        .entry(request.index)
                        .or_insert(BitVec::from_elem(block_info.num_blocks, false))
                        .set(request.get_block_index(), true);
                }
                request.write_to(stream)?;
                self.blocks_in_flight += 1;
                sent += 1;
            } else {
                info!("Got no block");
                break;
            }
        }
        Ok(sent)
    }
}

#[derive(Debug)]
pub struct CompletedPiece {
    pub index: usize,
    pub piece: Vec<u8>,
}

pub struct PieceInFlight {
    pub index: usize,
    piece: Vec<u8>,
    pub have: BitVec,
    last_block_length: usize,
    num_blocks: usize,
    blocks_received: usize,
}

impl PieceInFlight {
    const BLOCK_SIZE: usize = 1 << 14; // 16 KiB

    pub fn new(piece_size: usize, index: usize) -> PieceInFlight {
        let (num_blocks, last_block_length) = crate::math::get_block_info(piece_size);
        let mut piece = Vec::new();
        piece.reserve(piece_size);
        unsafe { piece.set_len(piece_size) }
        let have = BitVec::from_elem(num_blocks as usize, false);
        PieceInFlight {
            index,
            piece,
            have,
            last_block_length,
            num_blocks,
            blocks_received: 0,
        }
    }

    pub fn add_block_fn<F: FnOnce(&mut [u8]) -> Result<(), std::io::Error>>(
        &mut self,
        index: usize,
        begin: usize,
        length: usize,
        func: F,
    ) -> Result<Option<CompletedPiece>, std::io::Error> {
        debug_assert!(index == self.index);
        debug_assert!(self.piece.len() != 0);
        if begin % Self::BLOCK_SIZE != 0 {
            panic!("Error! Begin is in between blocks!");
        }
        let block_index = begin / Self::BLOCK_SIZE;
        if block_index >= self.num_blocks {
            panic!("Error! Out of range");
        }
        if self.have[block_index] {
            return Ok(None);
        }
        let block_length = if block_index == self.num_blocks - 1 {
            self.last_block_length
        } else {
            Self::BLOCK_SIZE
        };
        if block_length != length {
            panic!("Error in block length");
        }
        self.blocks_received += 1;
        debug!("Received block {} of {}", block_index + 1, self.num_blocks);
        let end = begin + length;
        func(&mut self.piece[begin..end])?;
        self.have.set(block_index, true);
        if self.blocks_received == self.num_blocks {
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
