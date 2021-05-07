use std::collections::VecDeque;
use std::sync::mpsc::Receiver;

use bit_vec::BitVec;
use log::info;
use rand::seq::SliceRandom;
use rand::thread_rng;

use crate::messages::Request;
use crate::piece_info::PieceInfo;

pub struct PieceAssigner {
    piece_info: PieceInfo,
    pieces: VecDeque<usize>,
    left: BitVec,
    failed_hash: Receiver<(usize, usize)>,
    endgame: bool,
    current_piece: Option<usize>,
    current_block: usize,
}

pub enum AssignedBlockResult {
    AssignedBlock { request: Request },
    EnterEndgame,
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
        }
    }

    pub fn get_block(&mut self, peer_has: &BitVec, _connection_id: usize) -> AssignedBlockResult {
        if self.pieces.len() == 0 && self.current_piece.is_none() {
            while let Ok((value, _length)) = self.failed_hash.try_recv() {
                self.pieces.push_back(value);
            }
            if self.pieces.len() == 0 {
                info!("Entering endgame mode");
                self.endgame = true;
                return AssignedBlockResult::EnterEndgame;
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
