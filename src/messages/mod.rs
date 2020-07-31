// TODO: change some of the panics to errors and propagate them up: they should result in
// disconnect and banning of peers.

pub mod message;
pub use message::*;

pub mod handshake;
pub use handshake::*;

pub mod keep_alive;
pub use keep_alive::*;

pub mod have;
pub use have::*;

pub mod bitfield;
pub use bitfield::*;

pub mod request;
pub use request::*;

pub mod block;
pub use block::*;

pub mod cancel;
pub use cancel::*;

use super::Connection;
use super::UpdateResult;
use super::UpdateSuccess;
use std::io::Error;
use std::io::Read;
use std::io::Write;
macro_rules! ImplSingleByteMessage {
    ($NAME:ident, $ID:literal, $Flag:ident, $Value:literal) => {
        #[derive(Debug, Clone)]
        pub struct $NAME {}

        impl Message for $NAME {
            const ID: i8 = $ID;
            const SIZE: MessageLength = MessageLength::Fixed(1);
            const NAME: &'static str = stringify!($NAME);

            fn update(self, connection: &mut Connection) -> UpdateResult {
                connection.$Flag = $Value;
                Ok(UpdateSuccess::Success)
            }

            fn read_data<T: Read>(_: &mut T, _: u32) -> Result<Self, Error> {
                Ok($NAME {})
            }
        }
    };
}
ImplSingleByteMessage!(Choke, 0, peer_choking, true);
ImplSingleByteMessage!(Unchoke, 1, peer_choking, false);
ImplSingleByteMessage!(Interested, 2, peer_interested, true);
ImplSingleByteMessage!(NotInterested, 3, peer_interested, false);

#[derive(Debug, Clone)]
pub struct Port {
    listen_port: u16,
}

impl Message for Port {
    const ID: i8 = 9;
    const SIZE: MessageLength = MessageLength::Fixed(3);
    const NAME: &'static str = "Port";

    fn read_data<T: Read>(reader: &mut T, _: u32) -> Result<Self, Error> {
        let listen_port = read_u16(reader)?;
        Ok(Port { listen_port })
    }

    fn update(self, _connection: &mut Connection) -> UpdateResult {
        // Simple ack, DHT not implemented
        Ok(UpdateSuccess::Success)
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&self.listen_port.to_be_bytes())?;
        Ok(())
    }
}

// TODO genericize these fuction if possible
fn read_u16<T: Read>(reader: &mut T) -> Result<u16, Error> {
    let mut buffer = [0; 2];
    reader.read_exact(&mut buffer)?;
    Ok(u16::from_be_bytes(buffer))
}

fn read_u32<T: Read>(reader: &mut T) -> Result<u32, Error> {
    let mut buffer = [0; 4];
    reader.read_exact(&mut buffer)?;
    Ok(u32::from_be_bytes(buffer))
}

fn to_u32_be(value: usize) -> [u8; 4] {
    (value as u32).to_be_bytes()
}
