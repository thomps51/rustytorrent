use log::info;
use std::io::Error;
use std::io::Read;
use std::io::Write;

use super::read_as_be;
use super::to_u32_be;
use super::Message;
use super::MessageLength;
use crate::client::piece_info::PieceInfo;
use crate::client::{EstablishedConnection, UpdateResult, UpdateSuccess};
use crate::common::BLOCK_LENGTH;

#[derive(Debug, Clone, PartialEq, PartialOrd, Eq, Ord)]
pub struct Request {
    index: usize,
    begin: usize,
    length: usize,
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

    fn update(self, connection: &mut EstablishedConnection) -> UpdateResult {
        log::debug!("Updating connection with request: {:?}", self);
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
    pub fn new(block_index: usize, piece_index: usize, piece_info: PieceInfo) -> Self {
        Request {
            index: piece_index,
            begin: block_index * BLOCK_LENGTH,
            length: piece_info.get_block_length(block_index, piece_index),
        }
    }

    pub fn block_index(&self) -> usize {
        self.begin / BLOCK_LENGTH
    }
    pub fn piece_index(&self) -> usize {
        self.index
    }

    pub fn offset(&self) -> usize {
        self.begin
    }
}
