use std::io::Error;
use std::io::Read;
use std::io::Write;

use bit_vec::BitVec;

use super::Message;
use super::MessageLength;
use crate::client::{EstablishedConnection, UpdateResult, UpdateSuccess};

#[derive(Debug, Clone)]
pub struct Bitfield {
    pub bitfield: BitVec,
}

impl Message for Bitfield {
    const ID: i8 = 5;
    const SIZE: MessageLength = MessageLength::Variable;
    const NAME: &'static str = "Bitfield";

    fn length(&self) -> usize {
        // 1 + ceil(bits/8)
        1 + ((self.bitfield.len() + 7) / 8)
    }

    fn read_data<T: Read>(reader: &mut T, length: usize) -> Result<Self, Error> {
        let mut buffer = Vec::new();
        let size = length - 1; // Subtract the ID byte
        buffer.resize(size as _, 0);
        reader.read_exact(&mut buffer)?;
        let result = Bitfield {
            bitfield: BitVec::from_bytes(&buffer),
        };
        Ok(result)
    }

    fn update(self, connection: &mut EstablishedConnection) -> UpdateResult {
        // Received bitfield was padded with extra bits, so we need to truncate it
        connection.peer_has = self.bitfield;
        connection.peer_has.truncate(connection.num_pieces);
        Ok(UpdateSuccess::Success)
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        let bytes = self.bitfield.to_bytes();
        writer.write_all(&bytes)?;
        Ok(())
    }
}
