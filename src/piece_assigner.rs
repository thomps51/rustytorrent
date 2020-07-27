// simple, monotonic count for now, should be "random", and be able to response if a peer does
// not have the piece that is assigned.

// Needs to also give size, since last piece will have a different size
pub struct PieceAssigner {
    counter: usize,
    total_pieces: usize,
    total_size: usize,
    piece_size: usize,
}

impl PieceAssigner {
    pub fn new(total_pieces: usize, total_size: usize, piece_size: usize) -> Self {
        Self {
            counter: 0,
            total_pieces,
            total_size,
            piece_size,
        }
    }

    // index, size
    // Needs to take into account which pieces that a peer has
    pub fn get(&mut self) -> (usize, usize) {
        let prev = self.counter;
        if prev >= self.total_pieces {
            panic!("requested too many pieces!");
        }
        let size = if prev == self.total_pieces - 1 {
            println!("Last piece: {}", prev);
            self.total_size % self.piece_size
        } else {
            self.piece_size
        };
        println!("PieceAssigner piece {} of size {}", prev, size);
        self.counter += 1;
        (prev, size)
    }

    pub fn has_pieces(&self) -> bool {
        self.counter != self.total_pieces
    }
}
