use std::collections::hash_map::Entry::Occupied;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use bit_vec::BitVec;
use log::{debug, info};
use rand::seq::SliceRandom;
use rand::thread_rng;

use super::piece_info::PieceInfo;
use crate::messages::Request;

type ConnectionId = usize;
type PieceId = usize;
type BlockId = usize;

pub struct PieceAssigner {
    pub piece_info: PieceInfo,
    pieces: VecDeque<usize>,
    left: BitVec,
    endgame: bool,
    current_piece: Option<usize>,
    current_block: usize,
    assigned: HashMap<ConnectionId, HashMap<PieceId, HashSet<BlockId>>>,
    endgame_unreceived_blocks: Vec<Request>,
    prev_unreceived_call: Instant,
}

pub enum AssignedBlockResult {
    AssignedBlock { request: Request },
    EndgameAssignedBlock { request: Request },
    NoBlocksToAssign,
}

// PieceAssigner is shared among all connections and starts out with a Deque of all pieces, and
// assigns them to connections as they are requested.  When all pieces are assigned, it goes into
// 'endgame mode'

impl PieceAssigner {
    pub fn new(piece_info: PieceInfo, have: &BitVec) -> Self {
        let mut pieces = Vec::with_capacity(piece_info.total_pieces);
        for (piece_index, value) in have.iter().enumerate() {
            if !value {
                pieces.push(piece_index)
            }
        }
        info!("PieceAssigner: {} pieces to assign", pieces.len());
        pieces.shrink_to_fit();
        pieces.shuffle(&mut thread_rng());
        Self {
            piece_info,
            pieces: pieces.into(),
            left: BitVec::from_elem(piece_info.total_pieces, true),
            endgame: false,
            current_piece: None,
            current_block: 0,
            assigned: HashMap::new(),
            endgame_unreceived_blocks: Vec::new(),
            prev_unreceived_call: Instant::now(),
        }
    }

    pub fn add_piece(&mut self, piece_index: usize) {
        self.pieces.push_back(piece_index);
    }

    pub fn clear(&mut self) {
        self.assigned.clear();
    }

    pub fn get_block<F: Fn() -> Vec<Request>>(
        &mut self,
        peer_has: &BitVec,
        connection_id: usize,
        unreceived: F,
    ) -> AssignedBlockResult {
        if !self.endgame_unreceived_blocks.is_empty() {
            self.endgame = true;
            let pieces_assigned = self.assigned.entry(connection_id).or_default();
            let mut request = None;
            for (pos, block_request) in self.endgame_unreceived_blocks.iter().enumerate() {
                if let Occupied(entry) = pieces_assigned.entry(block_request.piece_index()) {
                    if entry.get().contains(&block_request.block_index()) {
                        continue;
                    }
                }
                pieces_assigned
                    .entry(block_request.piece_index())
                    .or_default()
                    .insert(block_request.block_index());
                request = Some((pos, block_request.clone()));
                break;
            }
            if request.is_none() {
                info!(
                    "Connection {} has already requested all unreceived blocks once",
                    connection_id
                );
                return AssignedBlockResult::NoBlocksToAssign;
            }
            let (pos, request) = request.unwrap();
            self.endgame_unreceived_blocks.remove(pos);
            return AssignedBlockResult::EndgameAssignedBlock { request };
        }
        if self.pieces.is_empty() && self.current_piece.is_none() && self.pieces.is_empty() {
            // Throttle calls to unreceived because they are expensive and blocks will likely
            // be in flight
            if self.prev_unreceived_call.elapsed() < std::time::Duration::from_secs(1) {
                info!("Throttling blocks requests sent");
                return AssignedBlockResult::NoBlocksToAssign;
            }
            self.endgame_unreceived_blocks = unreceived();
            info!(
                "endgame unreceived blocks: {}",
                self.endgame_unreceived_blocks.len()
            );
            self.prev_unreceived_call = Instant::now();
            if self.endgame_unreceived_blocks.is_empty() {
                info!("No more unreceived blocks!");
                return AssignedBlockResult::NoBlocksToAssign;
            }
            return self.get_block(peer_has, connection_id, unreceived);
        }
        let current_piece = {
            if let Some(value) = self.current_piece {
                value
            } else if peer_has.all() {
                self.pieces.pop_front().unwrap()
            } else {
                // If the peer doesn't have all the pieces, we look for the first piece that we
                // haven't yet assigned that the peer does have.
                let mut intersection = peer_has.clone();
                intersection.and(&self.left);
                if let Some(index) = intersection.iter().position(|x| x) {
                    self.pieces.retain(|x| *x != index);
                    index
                } else {
                    debug!("Peer does not have any blocks for me to request");
                    return AssignedBlockResult::NoBlocksToAssign;
                }
            }
        };
        self.current_piece = Some(current_piece);
        self.left.set(current_piece, false);
        let current_block = self.current_block;
        if self.piece_info.is_last_block(current_block, current_piece) {
            self.current_piece = None;
            self.current_block = 0;
        } else {
            self.current_block += 1;
        }
        let request = Request::new(current_block, current_piece, self.piece_info);
        AssignedBlockResult::AssignedBlock { request }
    }

    pub fn is_endgame(&self) -> bool {
        self.endgame
    }
}
