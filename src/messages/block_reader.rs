use std::io::Read;

use crate::common::BLOCK_LENGTH;

// BlockReader makes sure that the data from the Block message is read from the read buffer.  It does
// this by enforcing that read() is called exactly once, either by the user or when the object is
// dropped.
//
// Originally this was just handled by a callback, but it was one of the most time consuming bugs to
// track down when it was not being called when a block had already been received, leaving the data
// in the read buffer to be interpreted as a new message.
pub struct BlockReader<'a, T: Read> {
    begin: usize,
    called: bool,
    index: usize,
    length: usize,
    reader: &'a mut T,
}

impl<'a, T: Read> BlockReader<'a, T> {
    pub fn new(reader: &'a mut T, begin: usize, index: usize, length: usize) -> Self {
        BlockReader {
            called: false,
            reader,
            length,
            index,
            begin,
        }
    }

    pub fn read(&mut self, dst: &mut [u8]) -> Result<(), std::io::Error> {
        debug_assert!(!self.called);
        debug_assert!(dst.len() == self.length);
        self.called = true;
        self.reader.read_exact(dst)?;
        Ok(())
    }

    pub fn block_index(&self) -> usize {
        self.begin / BLOCK_LENGTH
    }

    pub fn begin(&self) -> usize {
        self.begin
    }

    pub fn piece_index(&self) -> usize {
        self.index
    }

    pub fn len(&self) -> usize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<'a, T: Read> Drop for BlockReader<'a, T> {
    fn drop(&mut self) {
        if !self.called {
            std::io::copy(
                &mut self.reader.take(self.length as _),
                &mut std::io::sink(),
            )
            .unwrap();
        }
    }
}
