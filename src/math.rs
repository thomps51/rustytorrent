use crate::constants::BLOCK_LENGTH;

// Given piece length, calculate the (num_blocks, last_block_length), assuming usual fixed block
// length.
pub fn get_block_info(piece_length: usize) -> (usize, usize) {
    if piece_length % BLOCK_LENGTH == 0 {
        (piece_length / BLOCK_LENGTH, BLOCK_LENGTH)
    } else {
        (
            (piece_length / BLOCK_LENGTH) + 1,
            piece_length % BLOCK_LENGTH,
        )
    }
}
