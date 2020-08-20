use std::io::prelude::*;

use bit_vec::BitVec;
use log::debug;

use crate::messages::*;
use crate::piece_store::PieceStore;
use crate::SharedPieceAssigner;
use crate::SharedPieceStore;
use crate::MAX_OPEN_REQUESTS_PER_PEER;

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

    pub fn can_send_block_requests(&self) -> bool {
        self.blocks_in_flight < MAX_OPEN_REQUESTS_PER_PEER
    }

    pub fn num_blocks_to_request(&self) -> usize {
        MAX_OPEN_REQUESTS_PER_PEER - self.blocks_in_flight
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
                debug!("sending block request: {:?}", msg);
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
        piece.resize(piece_size, 0);
        let have = BitVec::from_elem(num_blocks as usize, false);
        PieceInFlight {
            index,
            piece,
            have,
            last_block_length,
            num_blocks,
            current_block: 0,
        }
    }

    pub fn add_block(&mut self, block: &Block) -> Option<CompletedPiece> {
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
        debug!("Received block {} of {}", block_index + 1, self.num_blocks);
        for i in 0..block_length {
            let offset = block.begin + i;
            self.piece[offset] = block.block[i];
        }
        self.have.set(block_index, true);
        if self.have.all() {
            debug!("Got piece {}", self.index);
            Some(CompletedPiece {
                index: self.index,
                piece: self.piece.clone(),
            })
        } else {
            None
        }
    }

    pub fn get_block_request(&mut self) -> Option<Request> {
        if self.current_block >= self.num_blocks {
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
