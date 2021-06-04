use std::collections::hash_map::Entry::Occupied;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::Receiver;
use std::time::Instant;

use bit_vec::BitVec;
use log::info;
use rand::seq::SliceRandom;
use rand::thread_rng;

use crate::messages::Request;
use crate::piece_info::PieceInfo;

type ConnectionId = usize;
type PieceId = usize;
type BlockId = usize;

pub struct PieceAssigner {
    pub piece_info: PieceInfo,
    pieces: VecDeque<usize>,
    left: BitVec,
    failed_hash: Receiver<(usize, usize)>,
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
    pub fn new(piece_info: PieceInfo, failed_hash: Receiver<(usize, usize)>) -> Self {
        let mut pieces = Vec::new();
        pieces.resize(piece_info.total_pieces, 0);
        for i in 0..piece_info.total_pieces {
            pieces[i] = i;
        }
        pieces.shuffle(&mut thread_rng());
        Self {
            piece_info,
            pieces: pieces.into(),
            left: BitVec::from_elem(piece_info.total_pieces, true),
            failed_hash,
            endgame: false,
            current_piece: None,
            current_block: 0,
            assigned: HashMap::new(),
            endgame_unreceived_blocks: Vec::new(),
            prev_unreceived_call: Instant::now(),
        }
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
        if self.endgame_unreceived_blocks.len() > 0 {
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
            if let None = request {
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
        if self.pieces.len() == 0 && self.current_piece.is_none() {
            while let Ok((value, _length)) = self.failed_hash.try_recv() {
                self.pieces.push_back(value);
            }
            if self.pieces.len() == 0 {
                // Throttle calls to unreceived because they are expensive and blocks will likely
                // be in flight
                if self.prev_unreceived_call.elapsed() < std::time::Duration::from_secs(1) {
                    return AssignedBlockResult::NoBlocksToAssign;
                }
                self.endgame_unreceived_blocks = unreceived();
                self.prev_unreceived_call = Instant::now();
                if self.endgame_unreceived_blocks.is_empty() {
                    info!("No more unreceived blocks!");
                    return AssignedBlockResult::NoBlocksToAssign;
                }
                return self.get_block(peer_has, connection_id, unreceived);
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
                        return AssignedBlockResult::NoBlocksToAssign;
                    }
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
        return self.endgame;
    }
}
