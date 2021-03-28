use std::io::Error;
use std::io::Read;
use std::io::Write;

use super::read_as_be;
use super::to_u32_be;
use super::Message;
use super::MessageLength;
use crate::connection::{Connection, UpdateResult};
use crate::constants::BLOCK_LENGTH;

#[derive(Clone)]
pub struct Block {
    pub index: usize,
    pub begin: usize,
    pub block: [u8; BLOCK_LENGTH],
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
        let index = read_as_be::<u32, _, usize>(reader)?;
        let begin = read_as_be::<u32, _, usize>(reader)?;
        let size = length - 9; // id byte, 2 4-byte sizes
        block_manager.add_block_fn(index, begin, size, |dst| {
            reader.read_exact(dst)?;
            Ok(())
        })?;
        Ok(())
    }
}
