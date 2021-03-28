use std::collections::HashMap;

use crate::constants::BLOCK_LENGTH;
use crate::messages::Cancel;
use bit_vec::BitVec;

pub fn reconcile<F: Fn(usize) -> BitVec>(
    get_block_have: F,
    sent: &mut HashMap<usize, BitVec>,
) -> (usize, Vec<Cancel>) {
    // each one that is removed needs to be cancelled.

    let mut to_remove = Vec::new();
    let mut to_cancel = Vec::new();
    for (piece_index, sent_blocks) in sent.iter_mut() {
        let mut have = get_block_have(*piece_index);
        have.and(&sent_blocks);
        for (block_index, value) in have.iter().enumerate() {
            if value {
                let begin = BLOCK_LENGTH * block_index;
                to_cancel.push(Cancel {
                    index: *piece_index,
                    begin,
                    length: BLOCK_LENGTH,
                });
                sent_blocks.set(block_index, false);
            }
        }
        if sent_blocks.none() {
            to_remove.push(*piece_index);
        }
    }
    for index in to_remove {
        sent.remove(&index);
    }
    let mut result = 0;
    for (_, sent_blocks) in sent {
        sent_blocks.iter().for_each(|x| result += x as usize);
    }
    (result, to_cancel)
}
