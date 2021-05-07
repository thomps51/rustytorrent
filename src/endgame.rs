use std::collections::HashMap;

use crate::constants::BLOCK_LENGTH;
use crate::messages::Cancel;
use bit_vec::BitVec;

pub fn reconcile<F: Fn(usize) -> BitVec>(
    get_block_have: F,
    have_pieces: BitVec,
    sent_block_requests: &mut HashMap<usize, BitVec>,
) -> (usize, Vec<Cancel>) {
    // each one that is removed needs to be cancelled.

    let mut to_remove = Vec::new();
    let mut to_cancel = Vec::new();
    for (piece_index, sent_blocks) in sent_block_requests.iter_mut() {
        if have_pieces[*piece_index] {
            sent_blocks.set_all();
            sent_blocks.negate();
        } else {
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
        }
        if sent_blocks.none() {
            to_remove.push(*piece_index);
        }
    }
    for index in to_remove {
        sent_block_requests.remove(&index);
    }
    let mut result = 0;
    for (_, sent_blocks) in sent_block_requests {
        sent_blocks.iter().for_each(|x| result += x as usize);
    }
    (result, to_cancel)
}
/*
#[cfg(test)]
mod tests {
    use crate::endgame::reconcile;
    use bit_vec::BitVec;
    use std::collections::HashMap;
    #[test]
    fn reconcile_have_and_sent_all_test() {
        let have_all = |_x: usize| BitVec::from_elem(10, true);
        let mut sent = HashMap::new();
        sent.insert(0, BitVec::from_elem(10, true));

        let (result, _) = reconcile(have_all, &mut sent);

        assert_eq!(result, 0);
    }
    #[test]
    fn reconcile_have_none_and_sent_all_test() {
        let have_none = |_x: usize| BitVec::from_elem(10, false);
        let mut sent = HashMap::new();
        sent.insert(0, BitVec::from_elem(10, true));

        let (result, _) = reconcile(have_none, &mut sent);

        assert_eq!(result, 10);
    }
    #[test]
    fn reconcile_have_none_and_sent_none_test() {
        let have_none = |_x: usize| BitVec::from_elem(10, false);
        let mut sent = HashMap::new();
        sent.insert(0, BitVec::from_elem(10, false));

        let (result, _) = reconcile(have_none, &mut sent);

        assert_eq!(result, 0);
    }
    #[test]
    fn reconcile_have_all_and_sent_none_test() {
        let have_all = |_x: usize| BitVec::from_elem(10, true);
        let mut sent = HashMap::new();
        sent.insert(0, BitVec::from_elem(10, false));

        let (result, _) = reconcile(have_all, &mut sent);

        assert_eq!(result, 0);
    }
}
*/
