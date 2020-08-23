use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::mpsc::Receiver;

use bit_set::BitSet;
use bit_vec::BitVec;
use log::debug;

use rand::seq::SliceRandom;
use rand::thread_rng;

pub struct PieceAssigner {
    total_pieces: usize,
    total_size: usize,
    piece_size: usize,
    pieces: VecDeque<usize>,
    left: BitVec,
    assigned: HashMap<usize, HashSet<usize>>,
    failed_hash: Receiver<usize>,
}

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
        }
    }

    // index, size
    pub fn get<F: Fn() -> BitVec>(
        &mut self,
        peer_has: &BitVec,
        received: F,
        connection_id: usize,
    ) -> Option<(usize, usize)> {
        let index = if self.pieces.len() == 0 {
            // Endgame mode: If we have assigned all pieces, but aren't yet finished, we should
            // start requesting missing pieces from other peers.  So we look for pieces that we
            // haven't received that the current peer has and we haven't already requested from that
            // peer.
            if let Ok(value) = self.failed_hash.try_recv() {
                debug!("Endgame mode: got failed hash index: {}", value);
                value
            } else {
                let mut empty = HashSet::new();
                let assigned = self.assigned.get_mut(&connection_id).unwrap_or(&mut empty);
                let mut intersection = received();
                intersection.negate();
                intersection.and(&peer_has);
                let intersection_set = BitSet::from_bit_vec(intersection);
                if let Some(index) = intersection_set.iter().find(|x| !assigned.contains(&x)) {
                    debug!("Endgame mode: assigning piece {}", index);
                    index
                } else {
                    return None;
                }
            }
        } else if peer_has.all() {
            self.pieces.pop_front().unwrap()
        } else {
            // If the peer doesn't have all the pieces, we look for the first piece that we
            // haven't yet assigned that the peer does have.
            let mut intersection = peer_has.clone();
            intersection.and(&self.left);
            if let Some(index) = intersection.iter().position(|x| x == true) {
                self.pieces.retain(|x| *x != index);
                self.pieces
                    .remove(self.pieces.iter().position(|x| *x == index).expect(""));
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
        Some((index, size))
    }
}
