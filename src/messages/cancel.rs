use std::io::Error;
use std::io::Read;
use std::io::Write;

use super::super::Connection;
use super::super::UpdateResult;
use super::super::UpdateSuccess;
use super::read_u32;
use super::Message;
use super::MessageLength;

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

    fn read_data<T: Read>(reader: &mut T, _: u32) -> Result<Self, Error> {
        let index = read_u32(reader)?;
        let begin = read_u32(reader)?;
        let length = read_u32(reader)?;
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
