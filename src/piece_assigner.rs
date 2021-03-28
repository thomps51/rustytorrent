use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::mpsc::Receiver;

use bit_vec::BitVec;
use log::{debug, info};
use rand::seq::SliceRandom;
use rand::thread_rng;

use crate::block_requester::{BlockAssigner, Mode};
use crate::math::get_block_info;
use crate::messages::Request;

pub struct PieceAssigner {
    total_pieces: usize,
    total_size: usize,
    piece_size: usize,
    pieces: VecDeque<usize>,
    left: BitVec,
    assigned: HashMap<usize, HashSet<usize>>,
    failed_hash: Receiver<usize>,
    pub mode: Mode,
    current_piece: Option<usize>,
    current_block: usize,
    endgame_unreceived_blocks: Vec<Request>,
    last_update: std::time::Instant,
}

pub struct BlockInfo {
    pub num_blocks: usize,
}

const PRINT_UPDATE_TIME: std::time::Duration = std::time::Duration::from_secs(5);

impl PieceAssigner {
    pub fn new(
        total_pieces: usize,
        total_size: usize,
        piece_size: usize,
        failed_hash: Receiver<usize>,
    ) -> Self {
        let mut pieces = Vec::new();
        pieces.resize(total_pieces, 0);
        for i in 0..total_pieces {
            pieces[i] = i;
        }
        pieces.shuffle(&mut thread_rng());
        Self {
            total_pieces,
            total_size,
            piece_size,
            pieces: pieces.into(),
            left: BitVec::from_elem(total_pieces, true),
            assigned: HashMap::new(),
            failed_hash,
            mode: Mode::Normal,
            current_piece: None,
            current_block: 0,
            endgame_unreceived_blocks: Vec::new(),
            last_update: std::time::Instant::now(),
        }
    }

    // index, size
    //
    // PieceAssigner could return a BlockAssigner that each connections gets blocks to send from.
    // Intially, each connections gets a different BlockAssigner (BlockRequester?), but in endgame
    // mode, we start returning BlockAssigners for pieces that aren't finished yet.  This implies I need to
    // store which BlockAssigners I have assigned, and close them when blocks all blocks.
    pub fn get<F: Fn() -> Vec<usize>>(
        &mut self,
        peer_has: &BitVec,
        unreceived: F,
        connection_id: usize,
    ) -> Option<BlockAssigner> {
        if self.pieces.len() == 0 {
            while let Ok(value) = self.failed_hash.try_recv() {
                self.pieces.push_back(value);
            }
            if self.pieces.len() == 0 {
                if self.mode == Mode::Endgame {
                    // Only assign pieces twice
                    return None;
                }
                let mut v = unreceived();
                v.shuffle(&mut thread_rng());
                self.pieces = v.into();
                self.mode = Mode::Endgame;
            }
        }
        let index = if peer_has.all() {
            self.pieces.pop_front().unwrap()
        } else {
            // If the peer doesn't have all the pieces, we look for the first piece that we
            // haven't yet assigned that the peer does have.
            let mut intersection = peer_has.clone();
            intersection.and(&self.left);
            if let Some(index) = intersection.iter().position(|x| x == true) {
                self.pieces.retain(|x| *x != index);
                index
            } else {
                return None;
            }
        };
        self.assigned
            .entry(connection_id)
            .or_default()
            .insert(index);
        self.left.set(index, false);
        let size = if index == self.total_pieces - 1 {
            let leftover = self.total_size % self.piece_size;
            if leftover == 0 {
                self.piece_size
            } else {
                leftover
            }
        } else {
            self.piece_size
        };
        Some(BlockAssigner::new(size, index, &self.mode))
    }

    // index, size
    //
    // PieceAssigner could return a BlockAssigner that each connections gets blocks to send from.
    // Intially, each connections gets a different BlockAssigner (BlockRequester?), but in endgame
    // mode, we start returning BlockAssigners for pieces that aren't finished yet.  This implies I need to
    // store which BlockAssigners I have assigned, and close them when blocks all blocks.
    pub fn get_block<F: Fn() -> Vec<Request>>(
        &mut self,
        peer_has: &BitVec,
        unreceived: F,
        connection_id: usize,
    ) -> Option<(Request, BlockInfo)> {
        //info!("Connection {} requesting block", connection_id);
        if self.endgame_unreceived_blocks.len() > 0 {
            //info!("Connection {} endgame request", connection_id);
            let request = self.endgame_unreceived_blocks.pop().unwrap();
            let length = if request.index == self.total_pieces - 1 {
                let leftover = self.total_size % self.piece_size;
                if leftover == 0 {
                    self.piece_size
                } else {
                    leftover
                }
            } else {
                self.piece_size
            };
            let (num_blocks, _) = get_block_info(length);
            //info!("Request: {:?}", request);
            let block_info = BlockInfo { num_blocks };
            return Some((request, block_info));
        }
        if self.pieces.len() == 0 && self.current_piece.is_none() {
            info!("Has no pieces");
            while let Ok(value) = self.failed_hash.try_recv() {
                self.pieces.push_back(value);
            }
            if self.pieces.len() == 0 {
                let now = std::time::Instant::now();
                if self.mode == Mode::Endgame && now - self.last_update < PRINT_UPDATE_TIME {
                    info!("Assigning second time, stopping...");
                    // Only assign blocks twice. TODO: do something better than this
                    return None;
                }
                self.last_update = now;
                self.endgame_unreceived_blocks = unreceived();
                info!(
                    "Got {} unreceived blocks",
                    self.endgame_unreceived_blocks.len()
                );
                self.mode = Mode::Endgame;
                return self.get_block(peer_has, unreceived, connection_id);
            }
        }
        let current_piece = {
            if let Some(value) = self.current_piece {
                value
            } else {
                if peer_has.all() {
                    self.pieces.pop_front().unwrap()
                } else {
                    // If the peer doesn't have all the pieces, we look for the first piece that we
                    // haven't yet assigned that the peer does have.
                    let mut intersection = peer_has.clone();
                    intersection.and(&self.left);
                    if let Some(index) = intersection.iter().position(|x| x == true) {
                        self.pieces.retain(|x| *x != index);
                        index
                    } else {
                        return None;
                    }
                }
            }
        };
        self.current_piece = Some(current_piece);
        //info!("Current piece: {} ", current_piece);
        self.left.set(current_piece, false);
        let length = if current_piece == self.total_pieces - 1 {
            let leftover = self.total_size % self.piece_size;
            if leftover == 0 {
                self.piece_size
            } else {
                leftover
            }
        } else {
            self.piece_size
        };
        let (num_blocks, last_block_length) = get_block_info(length);
        let current_block = self.current_block;
        let block_length = if current_block == num_blocks - 1 {
            self.current_piece = None;
            self.current_block = 0;
            last_block_length
        } else {
            self.current_block += 1;
            crate::constants::BLOCK_LENGTH
        };
        let request = Request {
            index: current_piece,
            begin: current_block * crate::constants::BLOCK_LENGTH,
            length: block_length,
        };
        //info!("Request: {:?}", request);
        let block_info = BlockInfo { num_blocks };
        Some((request, block_info))
    }
}
