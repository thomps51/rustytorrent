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
pub struct Request {
    pub index: usize,
    pub begin: usize,
    pub length: usize,
}

impl Message for Request {
    const ID: i8 = 6;
    const SIZE: MessageLength = MessageLength::Fixed(13);
    const NAME: &'static str = "Request";

    fn read_data<T: Read>(reader: &mut T, _: usize) -> Result<Self, Error> {
        let index = read_u32(reader)? as usize;
        let begin = read_u32(reader)? as usize;
        let length = read_u32(reader)? as usize;
        Ok(Request {
            index,
            begin,
            length,
        })
    }

    fn update(self, connection: &mut Connection) -> UpdateResult {
        connection.pending_peer_requests.push(self);
        Ok(UpdateSuccess::Success)
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&to_u32_be(self.index))?;
        writer.write_all(&to_u32_be(self.begin))?;
        writer.write_all(&to_u32_be(self.length))?;
        Ok(())
    }
}
