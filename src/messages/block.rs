use std::io::Error;
use std::io::Read;
use std::io::Write;
use std::mem::MaybeUninit;

use super::read_as_be;
use super::to_u32_be;
use super::Message;
use super::MessageLength;
use crate::connection::{Connection, UpdateResult, UpdateSuccess};

pub const BLOCK_SIZE: usize = 1 << 14; // 16 KiB

#[derive(Clone)]
pub struct Block {
    pub index: usize,
    pub begin: usize,
    pub block: [u8; BLOCK_SIZE],
}

impl Message for Block {
    const ID: i8 = 7;
    const SIZE: MessageLength = MessageLength::Variable;
    const NAME: &'static str = "Block";

    fn length(&self) -> usize {
        9 + self.block.len()
    }

    fn read_data<T: Read>(reader: &mut T, length: usize) -> Result<Self, Error> {
        let index: u32 = read_as_be(reader)?;
        let begin: u32 = read_as_be(reader)?;
        // Idea: instead of reading into a temp stack array, change it so that we can read directly
        // into the buffer we use for pieces.  This would avoid us having to pass this around and
        // then copy it into that buffer.  This would be quite a change and make this message a
        // special case.
        let block: [MaybeUninit<u8>; BLOCK_SIZE] = unsafe { MaybeUninit::uninit().assume_init() };
        let mut block = unsafe { std::mem::transmute::<_, [u8; BLOCK_SIZE]>(block) };
        let size = length - 9; // id byte, 2 4-byte sizes
        assert!(size <= BLOCK_SIZE);
        reader.read_exact(&mut block)?;
        Ok(Block {
            index: index as usize,
            begin: begin as usize,
            block,
        })
    }

    fn update(self, connection: &mut Connection) -> UpdateResult {
        connection.block_manager.add_block(self);
        Ok(UpdateSuccess::Success)
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&to_u32_be(self.index))?;
        writer.write_all(&to_u32_be(self.begin))?;
        writer.write_all(&self.block)?;
        Ok(())
    }
}
