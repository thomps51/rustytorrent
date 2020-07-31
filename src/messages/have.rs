use std::io::Error;
use std::io::Read;
use std::io::Write;

use super::super::Connection;
use super::super::UpdateError;
use super::super::UpdateResult;
use super::super::UpdateSuccess;
use super::read_u32;
use super::Message;
use super::MessageLength;

#[derive(Debug, Clone)]
pub struct Have {
    index: u32,
}

impl Message for Have {
    const ID: i8 = 4;
    const SIZE: MessageLength = MessageLength::Fixed(5);
    const NAME: &'static str = "Have";

    fn read_data<T: Read>(reader: &mut T, _: u32) -> Result<Self, Error> {
        let index = read_u32(reader)?;
        Ok(Have { index })
    }
    fn update(self, connection: &mut Connection) -> UpdateResult {
        if self.index as usize >= connection.peer_has.len() {
            return Err(UpdateError::IndexOutOfBounds);
        }
        connection.peer_has.set(self.index as usize, true);
        Ok(UpdateSuccess::Success)
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&self.index.to_be_bytes())?;
        Ok(())
    }
}
