use std::io::Error;
use std::io::Read;
use std::io::Write;

use super::super::Connection;
use super::super::UpdateResult;
use super::super::UpdateSuccess;
use super::Message;
use super::MessageLength;

#[derive(Debug, Clone)]
pub struct KeepAlive {}

impl Message for KeepAlive {
    const ID: i8 = -1; // N/A
    const SIZE: MessageLength = MessageLength::Fixed(0);
    const NAME: &'static str = "KeepAlive";

    fn read_data<T: Read>(_: &mut T, _: usize) -> Result<Self, Error> {
        Ok(KeepAlive {})
    }

    fn update(self, connection: &mut Connection) -> UpdateResult {
        connection.received_keep_alive();
        Ok(UpdateSuccess::Success)
    }

    fn write_to<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&[0])?;
        Ok(())
    }
}
