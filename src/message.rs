use crate::connection::Connection;
use crate::connection::UpdateError;
use crate::connection::UpdateResult;
use crate::connection::UpdateSuccess;
use bit_vec::BitVec;
use std::io::Error;
use std::io::Read;
use std::io::Write;

// TODO genericize these fuction if possible
fn read_u32<T: Read>(reader: &mut T) -> Result<u32, Error> {
    let mut buffer = [0; 4];
    reader.read_exact(&mut buffer)?;
    Ok(u32::from_be_bytes(buffer))
}

fn read_u16<T: Read>(reader: &mut T) -> Result<u16, Error> {
    let mut buffer = [0; 2];
    reader.read_exact(&mut buffer)?;
    Ok(u16::from_be_bytes(buffer))
}

fn read_byte<T: Read>(reader: &mut T) -> Result<u8, std::io::Error> {
    let mut buffer = [0; 1];
    reader.read_exact(&mut buffer)?;
    Ok(buffer[0])
}

fn verify_pstr<T: Read>(reader: &mut T) {
    let size = read_byte(reader).unwrap();
    assert_eq!(size as usize, Handshake::PSTR.len());
    let mut buffer = [0; Handshake::PSTR.len()];
    reader.read_exact(&mut buffer).unwrap();
    assert_eq!(&buffer, Handshake::PSTR);
}

#[derive(Debug, Clone, Default)]
pub struct Handshake {
    pub info_hash: crate::hash::Sha1Hash,
    pub peer_id: crate::PeerId,
}

impl Handshake {
    const PSTR: &'static [u8] = "BitTorrent protocol".as_bytes();
    const SIZE: u8 = 49 + Self::PSTR.len() as u8;

    pub fn new(peer_id: &str, info_hash: &crate::hash::Sha1Hash) -> Self {
        let mut result: Self = Default::default();
        result.info_hash = *info_hash;
        result.peer_id.copy_from_slice(peer_id.as_bytes());
        result
    }

    pub fn read_from<T: Read>(reader: &mut T) -> Result<Self, Error> {
        verify_pstr(reader);
        let mut info_hash = [0; crate::hash::SHA1_HASH_LENGTH];
        let mut peer_id = [0; crate::hash::SHA1_HASH_LENGTH];
        let mut reserved = [0; 8];
        reader.read_exact(&mut reserved)?;
        reader.read_exact(&mut info_hash)?;
        reader.read_exact(&mut peer_id)?;
        Ok(Handshake { info_hash, peer_id })
    }

    fn update(self, connection: &mut Connection) -> UpdateResult {
        connection.received_keep_alive();
        Ok(UpdateSuccess::Success)
    }

    pub fn write_to<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        /*
        writer.write_all(&[Self::SIZE])?;
        writer.write_all(Self::PSTR)?;
        writer.write_all(&[0, 0, 0, 0, 0, 0, 0, 0]);
        writer.write_all(&self.info_hash)?;
        writer.write_all(&self.peer_id)?;
        */
        let mut buffer = Vec::new();
        buffer.push(Self::PSTR.len() as u8);
        buffer.extend_from_slice(Self::PSTR);
        buffer.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        buffer.extend_from_slice(&self.info_hash);
        buffer.extend_from_slice(&self.peer_id);
        debug_assert_eq!(buffer.len() as u8, Self::SIZE);
        println!("Sending handshake {:?}", buffer);
        writer.write_all(&buffer)?;
        Ok(())
    }
}

pub enum MessageLength {
    Fixed(u32),
    Variable,
}

// TODO: change some of the panics to errors and propagate them up: they should result in
// disconnect and banning of peers.

// Default case will handle fixed length messages with no associated data, others will
// be custom defined.
pub trait Message: Sized + std::fmt::Debug {
    const ID: i8;
    const SIZE: MessageLength;
    const NAME: &'static str;

    fn length(&self) -> u32 {
        if let MessageLength::Fixed(length) = Self::SIZE {
            length
        } else {
            panic!("Variable length messages must define a length() function")
        }
    }

    fn length_be_bytes(&self) -> [u8; 4] {
        return self.length().to_be_bytes();
    }

    // When reading the message, we already know what we will be reading thanks to the id.  Therefore,
    // we should not read the id in ReadFrom.
    fn read_data<T: Read>(reader: &mut T, length: u32) -> Result<Self, Error>;

    fn read_from<T: Read>(reader: &mut T, length: u32) -> Result<Self, Error> {
        if let MessageLength::Fixed(expected) = Self::SIZE {
            if expected != length {
                panic!("Fixed-sized message received with wrong size");
            }
        }
        let message = Self::read_data(reader, length)?;
        println!("Read message of type: {}", Self::NAME);
        Ok(message)
    }

    fn update(self, connection: &mut Connection) -> UpdateResult;

    fn write_data<T: Write>(&self, _: &mut T) -> Result<(), Error> {
        debug_assert!(std::mem::size_of::<Self>() == 0);
        Ok(())
    }

    // Unlike ReadFrom, we don't know which particular message we have when we want
    fn write_to<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        println!("Writing message: {:?}", self);
        writer.write_all(&self.length_be_bytes())?;
        writer.write_all(&[Self::ID as u8])?;
        self.write_data(writer)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct KeepAlive {}

impl Message for KeepAlive {
    const ID: i8 = -1; // N/A
    const SIZE: MessageLength = MessageLength::Fixed(0);
    const NAME: &'static str = "KeepAlive";

    fn read_data<T: Read>(_: &mut T, _: u32) -> Result<Self, Error> {
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
pub struct Have {
    index: u32,
}

impl Message for Have {
    const ID: i8 = 4;
    const SIZE: MessageLength = MessageLength::Fixed(5);
    const NAME: &'static str = "Have";

    fn read_data<T: Read>(reader: &mut T, _: u32) -> Result<Self, Error> {
        let index = read_u32(reader)?;
        Ok(Have { index })
    }
    fn update(self, connection: &mut Connection) -> UpdateResult {
        if self.index as usize >= connection.peer_has.len() {
            return Err(UpdateError::IndexOutOfBounds);
        }
        connection.peer_has.set(self.index as usize, true);
        Ok(UpdateSuccess::Success)
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&self.index.to_be_bytes())?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Bitfield {
    bitfield: BitVec,
}

impl Message for Bitfield {
    const ID: i8 = 5;
    const SIZE: MessageLength = MessageLength::Variable;
    const NAME: &'static str = "Bitfield";

    fn read_data<T: Read>(reader: &mut T, length: u32) -> Result<Self, Error> {
        let mut buffer = Vec::new();
        let size = length - 1; // Subtract the ID byte
        buffer.resize(size as usize, 0);
        reader.read_exact(&mut buffer)?;
        Ok(Bitfield {
            bitfield: BitVec::from_bytes(&buffer),
        })
    }

    fn update(self, connection: &mut Connection) -> UpdateResult {
        connection.peer_has = self.bitfield;
        Ok(UpdateSuccess::Success)
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&self.bitfield.to_bytes())?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub index: u32,
    pub begin: u32,
    pub length: u32,
}

impl Message for Request {
    const ID: i8 = 6;
    const SIZE: MessageLength = MessageLength::Fixed(13);
    const NAME: &'static str = "Request";

    fn read_data<T: Read>(reader: &mut T, _: u32) -> Result<Self, Error> {
        let index = read_u32(reader)?;
        let begin = read_u32(reader)?;
        let length = read_u32(reader)?;
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
        writer.write_all(&self.index.to_be_bytes())?;
        writer.write_all(&self.begin.to_be_bytes())?;
        writer.write_all(&self.length.to_be_bytes())?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Block {
    pub index: u32,
    pub begin: u32,
    pub block: Vec<u8>,
}

impl Message for Block {
    const ID: i8 = 7;
    const SIZE: MessageLength = MessageLength::Variable;
    const NAME: &'static str = "Block";

    fn read_data<T: Read>(reader: &mut T, length: u32) -> Result<Self, Error> {
        let index = read_u32(reader)?;
        let begin = read_u32(reader)?;
        let mut block = Vec::new();
        let size = length - 9; // id byte, 2 4-byte sizes
        block.resize(size as usize, 0);
        reader.read_exact(&mut block)?;
        println!(
            "Received block for index {} at offset {} with size {}",
            index, begin, size
        );
        Ok(Block {
            index,
            begin,
            block,
        })
    }

    fn update(self, connection: &mut Connection) -> UpdateResult {
        match connection.block_manager.add_block(&self) {
            Some(piece) => Ok(UpdateSuccess::PieceComplete(piece)),
            None => Ok(UpdateSuccess::Success),
        }
    }

    fn write_data<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&self.index.to_be_bytes())?;
        writer.write_all(&self.begin.to_be_bytes())?;
        writer.write_all(&self.block)?;
        Ok(())
    }
}

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
