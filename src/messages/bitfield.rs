use std::io::Error;
use std::io::Read;
use std::io::Write;

use bit_vec::BitVec;
use log::debug;

use super::super::Connection;
use super::super::UpdateResult;
use super::super::UpdateSuccess;
use super::Message;
use super::MessageLength;

use crate::PieceStore;

#[derive(Debug, Clone)]
pub struct Bitfield {
    pub bitfield: BitVec,
}

impl Message for Bitfield {
    const ID: i8 = 5;
    const SIZE: MessageLength = MessageLength::Variable;
    const NAME: &'static str = "Bitfield";

    fn length(&self) -> usize {
        1 + self.bitfield.len()
    }

    fn read_data<T: Read>(reader: &mut T, length: usize) -> Result<Self, Error> {
        let mut buffer = Vec::new();
        let size = length - 1; // Subtract the ID byte
        buffer.resize(size as usize, 0);
        reader.read_exact(&mut buffer)?;
        Ok(Bitfield {
            bitfield: BitVec::from_bytes(&buffer),
        })
    }

    fn update(self, connection: &mut Connection) -> UpdateResult {
        // Received bitfield was padded with extra bits, so we need to truncate it
        connection.peer_has = self.bitfield;
        connection.peer_has.truncate(connection.num_pieces);
        Ok(UpdateSuccess::Success)
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&self.bitfield.to_bytes())?;
        Ok(())
    }
}
