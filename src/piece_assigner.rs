use std::collections::HashMap;
use std::collections::hash_map::Entry::{Occupied};
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::mpsc::Receiver;

use bit_vec::BitVec;
use log::{info};
use rand::seq::SliceRandom;
use rand::thread_rng;

use crate::math::get_block_info;
use crate::messages::Request;

type ConnectionId = usize;
type PieceId = usize;
type BlockId = usize;

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Endgame,
}

pub struct PieceAssigner {
    total_pieces: usize,
    total_size: usize,
    piece_size: usize,
    pieces: VecDeque<usize>,
    left: BitVec,
    assigned: HashMap<ConnectionId, HashMap<PieceId, HashSet<BlockId>>>,
    failed_hash: Receiver<usize>,
    pub mode: Mode,
    current_piece: Option<usize>,
    current_block: usize,
    endgame_unreceived_blocks: Vec<Request>,
}

pub struct BlockInfo {
    pub num_blocks: usize,
}

// PieceAssigner is shared among all connections and starts out with a Deque of all pieces, and
// assigns them to connections as they are requested.  When all pieces are assigned, it goes into
// 'endgame mode'

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
        }
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
        // Endgame blocks available
        if self.endgame_unreceived_blocks.len() > 0 {
            let pieces_assigned = self.assigned.entry(connection_id).or_default();
            let mut request = None;
            for (pos, block_request) in self.endgame_unreceived_blocks.iter().enumerate() {
                if let Occupied(entry) = pieces_assigned.entry(block_request.index) {
                    if entry.get().contains(&block_request.begin) {
                        continue
                    }
                }
                pieces_assigned.entry(block_request.index).or_default().insert(block_request.begin);
                request = Some((pos, block_request.clone()));
                break;
            };
            if let None = request {
                info!("Connection {} has already requested all unreceived blocks once", connection_id);
                return None
            }
            let (pos, request) = request.unwrap();
            self.endgame_unreceived_blocks.remove(pos);
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
            let block_info = BlockInfo { num_blocks };
            return Some((request, block_info));
        }
        if self.pieces.len() == 0 && self.current_piece.is_none() {
            info!("Has no pieces");
            while let Ok(value) = self.failed_hash.try_recv() {
                self.pieces.push_back(value);
            }
            if self.pieces.len() == 0 {
                self.endgame_unreceived_blocks = unreceived();
                if self.endgame_unreceived_blocks.is_empty() {
                    info!("No more unreceived blocks!");
                    return None
                }
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
