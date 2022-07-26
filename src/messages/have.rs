use std::io::Error;
use std::io::Read;
use std::io::Write;

use super::read_as_be;
use super::to_u32_be;
use super::Message;
use super::MessageLength;
use crate::client::{EstablishedConnection, UpdateError, UpdateResult, UpdateSuccess};

#[derive(Debug, Clone)]
pub struct Have {
    pub index: usize,
}

impl Message for Have {
    const ID: i8 = 4;
    const SIZE: MessageLength = MessageLength::Fixed(5);
    const NAME: &'static str = "Have";

    fn read_data<T: Read>(reader: &mut T, _: usize) -> Result<Self, Error> {
        let index = read_as_be::<u32, _, _>(reader)?;
        Ok(Have { index })
    }
    fn update(self, connection: &mut EstablishedConnection) -> UpdateResult {
        if self.index as usize >= connection.peer_has.len() {
            return Err(UpdateError::IndexOutOfBounds);
        }
        connection.peer_has.set(self.index, true);
        Ok(UpdateSuccess::Success)
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&to_u32_be(self.index))?;
        Ok(())
    }
}
