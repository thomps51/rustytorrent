use std::io::Error;
use std::io::Read;
use std::io::Write;

use super::read_as_be;
use super::Message;
use super::MessageLength;
use crate::connection::{Connection, UpdateResult, UpdateSuccess};

#[derive(Debug, Clone)]
pub struct Cancel {
    index: u32,
    begin: u32,
    length: u32,
}

impl Message for Cancel {
    const ID: i8 = 8;
    const SIZE: MessageLength = MessageLength::Fixed(13);
    const NAME: &'static str = "Cancel";

    fn read_data<T: Read>(reader: &mut T, _: usize) -> Result<Self, Error> {
        let index = read_as_be(reader)?;
        let begin = read_as_be(reader)?;
        let length = read_as_be(reader)?;
        Ok(Cancel {
            index,
            begin,
            length,
        })
    }

    fn update(self, connection: &mut Connection) -> UpdateResult {
        connection.pending_peer_cancels.push(self);
        Ok(UpdateSuccess::Success)
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&self.index.to_be_bytes())?;
        writer.write_all(&self.begin.to_be_bytes())?;
        writer.write_all(&self.length.to_be_bytes())?;
        Ok(())
    }
}
