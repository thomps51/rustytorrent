use std::io::prelude::*;

use bit_vec::BitVec;
use log::debug;

use crate::messages::*;
use crate::piece_store::PieceStore;
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
const MAX_OPEN_REQUESTS_PER_PEER: usize = 100;

// Manages requests and receipts of blocks.
pub struct BlockManager {
    blocks_in_flight: usize,
    piece_assigner: SharedPieceAssigner,
    piece_store: SharedPieceStore,
    piece_in_flight: Option<PieceInFlight>,
}

impl BlockManager {
    pub fn new(piece_assigner: SharedPieceAssigner, piece_store: SharedPieceStore) -> Self {
        BlockManager {
            blocks_in_flight: 0,
            piece_assigner,
            piece_store,
            piece_in_flight: None,
        }
    }
    pub fn add_block(&mut self, block: Block) {
        self.blocks_in_flight -= 1;
        self.piece_store.borrow_mut().write_block(block).unwrap();
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
    ) -> Result<(), std::io::Error> {
        while self.blocks_in_flight < MAX_OPEN_REQUESTS_PER_PEER {
            if let Some(current) = &mut self.piece_in_flight {
                if let Some(value) = current.get_block_request() {
                    debug!("sending block request: {:?}", value);
                    value.write_to(stream)?;
                    self.blocks_in_flight += 1;
                    continue;
                } else {
                    self.piece_in_flight = None;
                }
            }
            if let Some((piece_index, piece_length)) = self.piece_assigner.borrow_mut().get(
                &peer_has,
                || self.piece_store.borrow().have(),
                id,
            ) {
                let mut piece_in_flight = PieceInFlight::new(piece_length, piece_index);
                let msg = piece_in_flight.get_block_request().unwrap();
                debug!("sending new index block request: {:?}", msg);
                msg.write_to(stream)?;
                self.blocks_in_flight += 1;
                self.piece_in_flight = Some(piece_in_flight);
            } else {
                break;
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct CompletedPiece {
    pub index: usize,
    pub piece: Vec<u8>,
}

pub struct PieceInFlight {
    index: usize,
    piece: Vec<u8>,
    have: BitVec,
    last_block_length: usize,
    num_blocks: usize,
    current_block: usize,
    blocks_received: usize,
}

impl PieceInFlight {
    const BLOCK_SIZE: usize = 1 << 14; // 16 KiB

    pub fn new(piece_size: usize, index: usize) -> PieceInFlight {
        let (last_block_length, num_blocks) = if piece_size % Self::BLOCK_SIZE == 0 {
            (Self::BLOCK_SIZE, piece_size / Self::BLOCK_SIZE)
        } else {
            (
                piece_size % Self::BLOCK_SIZE,
                (piece_size / Self::BLOCK_SIZE) + 1,
            )
        };
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
            current_block: 0,
            blocks_received: 0,
        }
    }

    pub fn add_block(&mut self, block: &Block) -> Option<CompletedPiece> {
        if self.piece.len() == 0 {
            panic!("add_block() called after it returned a completed piece");
        }
        if block.index != self.index {
            panic!("Error! Block is for a different piece!");
        }
        if block.begin % Self::BLOCK_SIZE != 0 {
            panic!("Error! Begin is in between blocks!");
        }
        let block_index = block.begin / Self::BLOCK_SIZE;
        if block_index >= self.num_blocks {
            panic!("Error! Out of range");
        }
        let block_length = if block_index == self.num_blocks - 1 {
            self.last_block_length
        } else {
            Self::BLOCK_SIZE
        };
        if block_length > block.block.len() {
            panic!("Incorrect block length received");
        }
        if self.have[block_index] {
            return None;
        }
        self.blocks_received += 1;
        debug!("Received block {} of {}", block_index + 1, self.num_blocks);
        // This is safe since we have checked that block.begin and block_length are in bounds.
        unsafe {
            let dst_ptr = self.piece.as_mut_ptr().offset(block.begin as isize);
            let src_ptr = block.block.as_ptr();
            std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, block_length);
        }
        self.have.set(block_index, true);
        if self.blocks_received == self.num_blocks {
            debug!("Got piece {}", self.index);
            Some(CompletedPiece {
                index: self.index,
                piece: std::mem::replace(&mut self.piece, Vec::new()),
            })
        } else {
            None
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

    pub fn get_block_request(&mut self) -> Option<Request> {
        if self.current_block >= self.num_blocks {
            debug!(
                "get_block_request with index: {}, current_block: {}, num_blocks: {}",
                self.index, self.current_block, self.num_blocks
            );
            return None;
        }
        let begin = self.current_block * Self::BLOCK_SIZE;
        let length = if self.current_block == self.num_blocks - 1 {
            self.last_block_length
        } else {
            Self::BLOCK_SIZE
        };
        self.current_block += 1;
        Some(Request {
            index: self.index,
            begin,
            length,
        })
    }
}
