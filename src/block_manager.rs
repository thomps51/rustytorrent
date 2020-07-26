use std::cell::RefCell;
use std::io::prelude::*;
use std::rc::Rc;

use bit_vec::BitVec;

use crate::message::*;
use crate::MAX_OPEN_REQUESTS_PER_PEER;

// Manages requests and receipts of blocks.
pub struct BlockManager {
    blocks_in_flight: usize,
    piece_assigner: Rc<RefCell<crate::PieceAssigner>>,
    pieces_in_flight: Vec<PieceInFlight>,
}

impl BlockManager {
    pub fn new(piece_assigner: Rc<RefCell<crate::PieceAssigner>>) -> Self {
        BlockManager {
            blocks_in_flight: 0,
            piece_assigner,
            pieces_in_flight: Vec::new(),
        }
    }
    pub fn add_block(&mut self, block: &Block) -> Option<CompletedPiece> {
        self.blocks_in_flight -= 1;
        if let Some(index) = self
            .pieces_in_flight
            .iter()
            .position(|x| x.index == block.index)
        {
            let piece = &mut self.pieces_in_flight[index];
            if let Some(p) = piece.add_block(block) {
                self.pieces_in_flight.remove(index);
                Some(p)
            } else {
                None
            }
        } else {
            panic!("Got block for piece that wasn't requested");
        }
    }

    pub fn can_send_block_requests(&self) -> bool {
        self.blocks_in_flight < MAX_OPEN_REQUESTS_PER_PEER
    }

    pub fn send_block_requests<T: Write>(&mut self, stream: &mut T) -> Result<(), std::io::Error> {
        while self.blocks_in_flight < MAX_OPEN_REQUESTS_PER_PEER {
            if let Some(last) = self.pieces_in_flight.last_mut() {
                if let Some(value) = last.get_block_request() {
                    value.write_to(stream)?;
                    self.blocks_in_flight += 1;
                    continue;
                }
            }
            if !self.piece_assigner.borrow().has_pieces() {
                return Ok(());
            }
            let (piece_index, piece_length) = self.piece_assigner.borrow_mut().get();
            let mut piece_in_flight = PieceInFlight::new(piece_length, piece_index);
            if let Some(value) = piece_in_flight.get_block_request() {
                value.write_to(stream)?;
            } else {
                panic!("new piece didn't have any blocks");
            }
            self.pieces_in_flight.push(piece_in_flight);
            self.blocks_in_flight += 1;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct CompletedPiece {
    pub index: u32,
    pub piece: Vec<u8>,
}

struct PieceInFlight {
    index: u32,
    piece: Vec<u8>,
    have: BitVec,
    last_block_length: u32,
    num_blocks: u32,
    current_block: u32,
    blocks_in_flight: usize,
}

impl PieceInFlight {
    const BLOCK_SIZE: u32 = 1 << 14; // 16 KiB

    fn new(piece_size: u32, index: u32) -> PieceInFlight {
        let (last_block_length, num_blocks) = if piece_size % Self::BLOCK_SIZE == 0 {
            (Self::BLOCK_SIZE, piece_size / Self::BLOCK_SIZE)
        } else {
            (
                piece_size % Self::BLOCK_SIZE,
                (piece_size / Self::BLOCK_SIZE) + 1,
            )
        };
        let mut piece = Vec::new();
        piece.resize(piece_size as usize, 0);
        let have = BitVec::from_elem(num_blocks as usize, false);
        PieceInFlight {
            index,
            piece,
            have,
            last_block_length,
            num_blocks,
            current_block: 0,
            blocks_in_flight: 0,
        }
    }

    fn add_block(&mut self, block: &Block) -> Option<CompletedPiece> {
        self.blocks_in_flight -= 1;
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
        println!("Received block {} of {}", block_index + 1, self.num_blocks);
        for i in 0..block_length {
            let offset = block.begin + i;
            self.piece[offset as usize] = block.block[i as usize];
        }
        self.have.set(block_index as usize, true);
        if self.have.all() {
            Some(CompletedPiece {
                index: self.index,
                piece: self.piece.clone(),
            })
        } else {
            None
        }
    }

    fn get_block_request(&mut self) -> Option<Request> {
        if self.current_block >= self.num_blocks {
            return None;
        }
        if self.blocks_in_flight > 5 {
            return None;
        }
        self.blocks_in_flight += 1;
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
