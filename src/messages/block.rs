use std::io::Error;
use std::io::Read;
use std::io::Write;

use super::super::Connection;
use super::super::UpdateResult;
use super::super::UpdateSuccess;
use super::read_u32;
use super::to_u32_be;
use super::Message;
use super::MessageLength;

#[derive(Debug, Clone)]
pub struct Block {
    pub index: usize,
    pub begin: usize,
    pub block: Vec<u8>,
}

impl Message for Block {
    const ID: i8 = 7;
    const SIZE: MessageLength = MessageLength::Variable;
    const NAME: &'static str = "Block";

    fn length(&self) -> usize {
        9 + self.block.len()
    }

    fn read_data<T: Read>(reader: &mut T, length: usize) -> Result<Self, Error> {
        let index = read_u32(reader)? as usize;
        let begin = read_u32(reader)? as usize;
        let mut block = Vec::new();
        let size = length - 9; // id byte, 2 4-byte sizes
        block.resize(size as usize, 0);
        reader.read_exact(&mut block)?;
        Ok(Block {
            index,
            begin,
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
