use crate::common::BLOCK_LENGTH;

#[derive(Debug, Copy, Clone)]
pub struct PieceInfo {
    pub total_length: usize,
    pub piece_length: usize,
    pub total_pieces: usize,
}

impl PieceInfo {
    pub fn get_piece_length(&self, piece_index: usize) -> usize {
        if piece_index == self.total_pieces - 1 {
            let leftover = self.total_length % self.piece_length;
            if leftover == 0 {
                self.piece_length
            } else {
                leftover
            }
        } else {
            self.piece_length
        }
    }

    pub fn get_num_blocks(&self, piece_index: usize) -> usize {
        let piece_length = self.get_piece_length(piece_index);
        if piece_length % BLOCK_LENGTH == 0 {
            piece_length / BLOCK_LENGTH
        } else {
            (piece_length / BLOCK_LENGTH) + 1
        }
    }

    pub fn get_block_length(&self, block_index: usize, piece_index: usize) -> usize {
        let num_blocks = self.get_num_blocks(piece_index);
        let piece_length = self.get_piece_length(piece_index);
        if block_index == num_blocks - 1 {
            if piece_length % BLOCK_LENGTH == 0 {
                BLOCK_LENGTH
            } else {
                piece_length % BLOCK_LENGTH
            }
        } else {
            BLOCK_LENGTH
        }
    }

    pub fn is_last_block(&self, block_index: usize, piece_index: usize) -> bool {
        let num_blocks = self.get_num_blocks(piece_index);
        block_index == num_blocks - 1
    }
}
