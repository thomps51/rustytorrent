use log::debug;
use log::info;
use std::collections::HashMap;
use std::io::prelude::*;

use bit_vec::BitVec;

use super::piece_assigner::AssignedBlockResult;
use crate::common::SharedBlockCache;
use crate::common::SharedPieceAssigner;
use crate::messages::*;

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
    endgame_sent_blocks: HashMap<usize, BitVec>,
    pub block_cache: SharedBlockCache,
}

impl BlockManager {
    pub fn new(piece_assigner: SharedPieceAssigner, block_cache: SharedBlockCache) -> Self {
        BlockManager {
            blocks_in_flight: 0,
            piece_assigner,
            endgame_sent_blocks: HashMap::new(),
            block_cache,
        }
    }
    pub fn add_block<T: Read>(&mut self, data: BlockReader<T>) -> Result<(), std::io::Error> {
        if !self.piece_assigner.borrow().is_endgame() {
            // TODO: be smarter about this count and endgame in general
            self.blocks_in_flight -= 1;
        }
        let piece_index = data.piece_index();
        // If write_block fails, that means the hash failed and we need to add it back to piece_assigner
        if let Err(_) = self.block_cache.borrow_mut().write_block(data) {
            self.piece_assigner.borrow_mut().add_piece(piece_index);
        }
        Ok(())
    }

    pub fn send_block_requests<T: Write>(
        &mut self,
        stream: &mut T,
        peer_has: &BitVec,
        id: usize,
    ) -> Result<usize, std::io::Error> {
        let mut sent = 0;
        let mut is_endgame = false;
        let mut piece_assigner = self.piece_assigner.borrow_mut();
        while self.blocks_in_flight < MAX_OPEN_REQUESTS_PER_PEER {
            match piece_assigner.get_block(&peer_has, id, || {
                self.block_cache.borrow().endgame_get_unreceived_blocks()
            }) {
                AssignedBlockResult::NoBlocksToAssign => {
                    info!("no blocks to assign");
                    break;
                }
                AssignedBlockResult::AssignedBlock { request } => {
                    debug!("Assigned block: {:?}", request);
                    request.write_to(stream)?;
                }
                AssignedBlockResult::EndgameAssignedBlock { request } => {
                    debug!("Endgame assigned block: {:?}", request);
                    request.write_to(stream)?;
                    is_endgame = true;
                    self.endgame_sent_blocks
                        .entry(request.piece_index())
                        .or_insert(BitVec::from_elem(
                            piece_assigner
                                .piece_info
                                .get_num_blocks(request.piece_index()),
                            false,
                        ))
                        .set(request.block_index(), true);
                }
            }
            self.blocks_in_flight += 1;
            sent += 1;
        }
        if is_endgame {
            let (blocks_in_flight, cancels) = self
                .block_cache
                .borrow()
                .reconcile(&mut self.endgame_sent_blocks);
            self.blocks_in_flight = blocks_in_flight;
            for cancel in cancels {
                cancel.write_to(stream)?;
            }
        }
        Ok(sent)
    }
}
