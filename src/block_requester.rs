use std::cell::RefCell;
use std::rc::Rc;

use bit_vec::BitVec;
use log::info;
use rand::Rng;

use crate::constants::BLOCK_LENGTH;
use crate::messages::Request;

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Endgame,
}

pub struct BlockAssigner {
    index: usize,
    last_block_length: usize,
    pub num_blocks: usize,
    current_block: usize,
    mode: Mode,
    endgame_not_requested: BitVec,
    endgame_shared_requests: Rc<RefCell<Vec<Request>>>,
}

impl BlockAssigner {
    pub fn new(piece_length: usize, index: usize, mode: &Mode) -> Self {
        let (num_blocks, last_block_length) = crate::math::get_block_info(piece_length);
        BlockAssigner {
            index,
            last_block_length,
            num_blocks,
            current_block: 0,
            mode: (*mode).clone(),
            endgame_not_requested: BitVec::new(),
            endgame_shared_requests: RefCell::new(Vec::new()).into(),
        }
    }
    pub fn new_endgame(requests: Rc<RefCell<Vec<Request>>>) -> Self {
        BlockAssigner {
            index: 0,
            last_block_length: 0,
            num_blocks: 0,
            current_block: 0,
            mode: Mode::Endgame,
            endgame_not_requested: BitVec::new(),
            endgame_shared_requests: requests,
        }
    }

    fn get_block_length(&self, block_index: usize) -> usize {
        if block_index == self.num_blocks - 1 {
            self.last_block_length
        } else {
            BLOCK_LENGTH
        }
    }

    pub fn get_block_request<F: FnOnce(usize) -> BitVec>(
        &mut self,
        received: F,
    ) -> Option<Request> {
        if self.mode == Mode::Endgame {
            return self.endgame_shared_requests.borrow_mut().pop();
        }

        if self.current_block >= self.num_blocks {
            return None;
        }
        let begin = self.current_block * BLOCK_LENGTH;
        let length = self.get_block_length(self.current_block);
        self.current_block += 1;
        Some(Request {
            index: self.index,
            begin,
            length,
        })
    }

    pub fn is_endgame(&self) -> bool {
        if let Mode::Endgame = self.mode {
            true
        } else {
            false
        }
    }
}
