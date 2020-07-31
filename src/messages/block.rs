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

    fn read_data<T: Read>(reader: &mut T, length: u32) -> Result<Self, Error> {
        let index = read_u32(reader)? as usize;
        let begin = read_u32(reader)? as usize;
        let mut block = Vec::new();
        let size = length - 9; // id byte, 2 4-byte sizes
        block.resize(size as usize, 0);
        reader.read_exact(&mut block)?;
        println!(
            "Received block for index {} at offset {} with size {}",
            index, begin, size
        );
        Ok(Block {
            index,
            begin,
            block,
        })
    }

    fn update(self, connection: &mut Connection) -> UpdateResult {
        match connection.block_manager.add_block(&self) {
            Some(piece) => Ok(UpdateSuccess::PieceComplete(piece)),
            None => Ok(UpdateSuccess::Success),
        }
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&to_u32_be(self.index))?;
        writer.write_all(&to_u32_be(self.begin))?;
        writer.write_all(&self.block)?;
        Ok(())
    }
}
