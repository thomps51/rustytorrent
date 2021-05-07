use std::io::Error;
use std::io::Read;
use std::io::Write;

use super::read_as_be;
use super::to_u32_be;
use super::Message;
use super::MessageLength;
use crate::connection::{Connection, UpdateResult};

#[derive(Clone)]
pub struct Block {
    index: usize,
    begin: usize,
    block: Vec<u8>,
}

// BlockData makes sure that the data from the Block message is read from the read buffer.  It does
// this by enforcing that read() is called exactly once, either by the user or when the object is
// dropped.
//
// Originally this was just handled by a callback, but it was one of the most time consuming bugs to
// track down when it was not being called when a block had already been received, leaving the data
// in the read buffer to be interpreted as a new message.
pub struct BlockData<'a, T: Read> {
    called: bool,
    reader: &'a mut T,
    length: usize,
}

impl Message for Block {
    const ID: i8 = 7;
    const SIZE: MessageLength = MessageLength::Variable;
    const NAME: &'static str = "Block";

    fn length(&self) -> usize {
        9 + self.block.len()
    }

    fn read_data<T: Read>(_: &mut T, _: usize) -> Result<Self, Error> {
        panic!("read_and_update used instead");
    }

    fn update(self, _: &mut Connection) -> UpdateResult {
        panic!("read_and_update used instead");
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&to_u32_be(self.index))?;
        writer.write_all(&to_u32_be(self.begin))?;
        writer.write_all(&self.block)?;
        Ok(())
    }
}

impl Block {
    pub fn read_and_update<T: Read>(
        reader: &mut T,
        block_manager: &mut crate::block_manager::BlockManager,
        length: usize,
    ) -> Result<(), Error> {
        // Combining read and update allows us to send the data straight where it needs to go instead
        // of copying it into the buffer and then copying it to the store.
        let index = read_as_be::<u32, _, usize>(reader)?;
        let begin = read_as_be::<u32, _, usize>(reader)?;
        let size = length - 9; // id byte, 2 4-byte sizes
        block_manager.add_block(index, begin, size, BlockData::new(reader, size))?;
        Ok(())
    }
}

impl<'a, T: Read> BlockData<'a, T> {
    pub fn new(reader: &'a mut T, length: usize) -> Self {
        BlockData {
            called: false,
            reader,
            length,
        }
    }

    pub fn read(&mut self, dst: &mut [u8]) -> Result<(), std::io::Error> {
        debug_assert!(!self.called);
        self.called = true;
        self.reader.read_exact(dst)?;
        Ok(())
    }
}

impl<'a, T: Read> Drop for BlockData<'a, T> {
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
