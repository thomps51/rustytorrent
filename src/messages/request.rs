use std::io::Error;
use std::io::Read;
use std::io::Write;

use super::read_as_be;
use super::to_u32_be;
use super::Message;
use super::MessageLength;
use crate::connection::{Connection, UpdateResult, UpdateSuccess};

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
        let index = read_as_be::<u32, _, _>(reader)?;
        let begin = read_as_be::<u32, _, _>(reader)?;
        let length = read_as_be::<u32, _, _>(reader)?;
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

impl Request {
    pub fn get_block_index(&self) -> usize {
        self.begin / crate::constants::BLOCK_LENGTH
    }
}
