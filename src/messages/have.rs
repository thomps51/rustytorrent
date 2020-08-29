use std::io::Error;
use std::io::Read;
use std::io::Write;

use super::read_as_be;
use super::to_u32_be;
use super::Message;
use super::MessageLength;
use crate::connection::{Connection, UpdateError, UpdateResult, UpdateSuccess};

#[derive(Debug, Clone)]
pub struct Have {
    pub index: usize,
}

impl Message for Have {
    const ID: i8 = 4;
    const SIZE: MessageLength = MessageLength::Fixed(5);
    const NAME: &'static str = "Have";

    fn read_data<T: Read>(reader: &mut T, _: usize) -> Result<Self, Error> {
        let index: u32 = read_as_be(reader)?;
        Ok(Have {
            index: index as usize,
        })
    }
    fn update(self, connection: &mut Connection) -> UpdateResult {
        if self.index as usize >= connection.peer_has.len() {
            return Err(UpdateError::IndexOutOfBounds);
        }
        connection.peer_has.set(self.index as usize, true);
        Ok(UpdateSuccess::Success)
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&to_u32_be(self.index))?;
        Ok(())
    }
}
