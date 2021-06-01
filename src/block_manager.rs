use std::io::prelude::*;

use bit_vec::BitVec;

use crate::messages::*;
use crate::piece_assigner::AssignedBlockResult;
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
pub const MAX_OPEN_REQUESTS_PER_PEER: usize = 10;

// Manages requests and receipts of blocks.
pub struct BlockManager {
    pub blocks_in_flight: usize,
    pub piece_assigner: SharedPieceAssigner,
    piece_store: SharedPieceStore,
}

impl BlockManager {
    pub fn new(piece_assigner: SharedPieceAssigner, piece_store: SharedPieceStore) -> Self {
        BlockManager {
            blocks_in_flight: 0,
            piece_assigner,
            piece_store,
        }
    }
    pub fn add_block<T: Read>(&mut self, data: BlockReader<T>) -> Result<(), std::io::Error> {
        if !self.piece_assigner.borrow().is_endgame() {
            self.blocks_in_flight -= 1;
        }
        self.piece_store.borrow_mut().write_block(data)?;
        Ok(())
    }

    pub fn send_block_requests<T: Write>(
        &mut self,
        stream: &mut T,
        peer_has: &BitVec,
        id: usize,
    ) -> Result<usize, std::io::Error> {
        let mut sent = 0;
        let mut piece_assigner = self.piece_assigner.borrow_mut();
        while self.blocks_in_flight < MAX_OPEN_REQUESTS_PER_PEER {
            match piece_assigner.get_block(&peer_has, id) {
                AssignedBlockResult::NoBlocksToAssign => {
                    break;
                }
                AssignedBlockResult::EnterEndgame => {
                    break;
                }
                AssignedBlockResult::AssignedBlock { request } => {
                    request.write_to(stream)?;
                    self.blocks_in_flight += 1;
                    sent += 1;
                }
            }
        }
        if sent > 0 {
            stream.flush()?;
        }
        Ok(sent)
    }
}
