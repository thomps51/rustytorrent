use std::io::Error;
use std::io::Read;
use std::io::Write;

use bit_vec::BitVec;

use super::super::Connection;
use super::super::UpdateResult;
use super::super::UpdateSuccess;
use super::Message;
use super::MessageLength;

#[derive(Debug, Clone)]
pub struct Bitfield {
    bitfield: BitVec,
}

impl Message for Bitfield {
    const ID: i8 = 5;
    const SIZE: MessageLength = MessageLength::Variable;
    const NAME: &'static str = "Bitfield";

    fn read_data<T: Read>(reader: &mut T, length: u32) -> Result<Self, Error> {
        let mut buffer = Vec::new();
        let size = length - 1; // Subtract the ID byte
        buffer.resize(size as usize, 0);
        reader.read_exact(&mut buffer)?;
        Ok(Bitfield {
            bitfield: BitVec::from_bytes(&buffer),
        })
    }

    fn update(self, connection: &mut Connection) -> UpdateResult {
        connection.peer_has = self.bitfield;
        Ok(UpdateSuccess::Success)
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&self.bitfield.to_bytes())?;
        Ok(())
    }
}
