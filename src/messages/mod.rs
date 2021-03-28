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

use crate::connection::{Connection, UpdateResult, UpdateSuccess};
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

            fn read_data<T: Read>(_: &mut T, _: usize) -> Result<Self, Error> {
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

    fn read_data<T: Read>(reader: &mut T, _: usize) -> Result<Self, Error> {
        let listen_port = read_as_be::<u16, _, _>(reader)?;
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

pub trait Primative: Sized + std::marker::Copy {
    fn from_be_bytes(array: &[u8]) -> Self;
}

use std::convert::TryInto;
macro_rules! ImplPrimative {
    ($NAME:ident) => {
        impl Primative for $NAME {
            fn from_be_bytes(array: &[u8]) -> Self {
                $NAME::from_be_bytes(array.try_into().unwrap())
            }
        }
    };
}

ImplPrimative!(u16);
ImplPrimative!(u32);
ImplPrimative!(u128);
use num_traits::AsPrimitive;
use num_traits::PrimInt;
pub fn read_as_be<T: Primative + AsPrimitive<V>, U: Read, V: PrimInt + 'static>(
    reader: &mut U,
) -> Result<V, Error> {
    // I don't understand why 'static is necessary.  Shouldn't PrimInt make it not a reference?
    // Some how an immutable reference meets all that criteria.
    //
    // Ideally we would use an exact size array here, but until rust gets better const generic
    // support and we are able to do [u8; std::mem::size_of::<T>()] here, it's easier to resort to
    // using a max size.
    let mut buffer = [0; 128 / 8];
    let length = std::mem::size_of::<T>();
    if length > buffer.len() {
        panic!("Length too large! Only supports up to 128 bit types");
    }
    let slice = &mut buffer[0..length];
    reader.read_exact(slice)?;
    let result: V = T::from_be_bytes(slice).as_();
    Ok(result)
}

pub fn to_u32_be(value: usize) -> [u8; 4] {
    (value as u32).to_be_bytes()
}
