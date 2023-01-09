pub mod block_reader;
pub use block_reader::*;
pub mod handshake;
pub use handshake::*;
pub mod protocol_message;
pub use protocol_message::*;
pub mod udp_tracker;

use std::io::Read;

use crate::messages::protocol_message::HasId;

use bit_vec::BitVec;
use write_to::{Length, NormalizedIntegerAccessors, ReadFrom, WriteTo};

use crate::{
    client::{
        piece_info::PieceInfo, BlockManager, EstablishedConnection, UpdateError, UpdateResult,
        UpdateSuccess,
    },
    common::BLOCK_LENGTH,
};

pub fn read_byte<T: Read>(reader: &mut T) -> std::io::Result<u8> {
    let mut buffer = [0; 1];
    reader.read_exact(&mut buffer)?;
    Ok(buffer[0])
}

// macro_rules! derive_message {
//     ($ID: literal, $NAME:ident) => {
//         #[derive(Debug, WriteTo, ReadFrom, Length, NormalizedIntegerAccessors, Clone)]
//         pub struct $NAME {}

//         impl HasId for $NAME {
//             const ID: u8 = $ID;
//         }
//     };
//     ($ID: literal, $NAME:ident, $($element: ident: $ty: ty),*) => {
//         #[derive(Debug, WriteTo, ReadFrom, Length, NormalizedIntegerAccessors, Clone)]
//         pub struct $NAME {
//             $(pub $element: $ty),*
//         }

//         impl HasId for $NAME {
//             const ID: u8 = $ID;
//         }
//     };
// }

// Note, trying to condense these with macro_rules leads to NormalizedIntegetAccessors
// not working properly because of macro hygiene (doesn't implement a trait)
#[derive(Debug, ReadFrom, WriteTo, Length, Clone, NormalizedIntegerAccessors)]
pub struct Choke {}

#[derive(Debug, ReadFrom, WriteTo, Length, Clone, NormalizedIntegerAccessors)]
pub struct Unchoke {}

#[derive(Debug, ReadFrom, WriteTo, Length, Clone, NormalizedIntegerAccessors)]
pub struct Interested {}

#[derive(Debug, ReadFrom, WriteTo, Length, Clone, NormalizedIntegerAccessors)]
pub struct NotInterested {}

#[derive(Debug, ReadFrom, WriteTo, Length, Clone, NormalizedIntegerAccessors)]
pub struct Bitfield {
    pub bitfield: BitVec,
}

#[derive(Debug, ReadFrom, WriteTo, Length, Clone, NormalizedIntegerAccessors)]
pub struct Have {
    pub index: u32,
}

#[derive(Debug, ReadFrom, WriteTo, Clone, NormalizedIntegerAccessors)]
pub struct Request {
    pub index: u32,
    pub begin: u32,
    pub length: u32,
}

#[derive(Debug, ReadFrom, WriteTo, Length, Clone, NormalizedIntegerAccessors)]
pub struct Block {
    pub index: u32,
    pub begin: u32,
    pub block: Vec<u8>,
}

#[derive(Debug, ReadFrom, WriteTo, Length, Clone, NormalizedIntegerAccessors)]
pub struct Cancel {
    pub index: u32,
    pub begin: u32,
    pub length: u32,
}

#[derive(Debug, ReadFrom, WriteTo, Length, Clone, NormalizedIntegerAccessors)]
pub struct Port {
    pub listen_port: u16,
}

macro_rules! impl_has_id {
    ($ID: literal, $NAME:ident) => {
        impl HasId for $NAME {
            const ID: u8 = $ID;
        }
    };
}

impl_has_id!(0, Choke);
impl_has_id!(1, Unchoke);
impl_has_id!(2, Interested);
impl_has_id!(3, NotInterested);
impl_has_id!(4, Have);
impl_has_id!(5, Bitfield);
impl_has_id!(6, Request);
impl_has_id!(7, Block);
impl_has_id!(8, Cancel);
impl_has_id!(9, Port);

macro_rules! impl_length_fixed_size {
    ($NAME:ident) => {
        impl Length for $NAME {
            fn length(&self) -> usize {
                std::mem::size_of::<$NAME>()
            }
        }
    };
}

impl_length_fixed_size!(Request);

macro_rules! ImplSingleByteMessage {
    ($NAME:ident, $Flag:ident, $Value:literal) => {
        impl ProtocolMessage for $NAME {
            fn update(self, connection: &mut EstablishedConnection) -> UpdateResult {
                connection.$Flag = $Value;
                Ok(UpdateSuccess::Success)
            }
        }
    };
}
ImplSingleByteMessage!(Choke, peer_choking, true);
ImplSingleByteMessage!(Unchoke, peer_choking, false);
ImplSingleByteMessage!(Interested, peer_interested, true);
ImplSingleByteMessage!(NotInterested, peer_interested, false);

impl ProtocolMessage for Have {
    fn update(self, connection: &mut EstablishedConnection) -> UpdateResult {
        if self.index as usize >= connection.peer_has.len() {
            return Err(UpdateError::IndexOutOfBounds);
        }
        connection.peer_has.set(self.index as usize, true);
        Ok(UpdateSuccess::Success)
    }
}

impl ProtocolMessage for Bitfield {
    fn update(self, connection: &mut EstablishedConnection) -> UpdateResult {
        // Received bitfield was padded with extra bits, so we need to truncate it
        connection.peer_has = self.bitfield;
        connection.peer_has.truncate(connection.num_pieces);
        Ok(UpdateSuccess::Success)
    }
}

impl ProtocolMessage for Request {
    fn update(self, connection: &mut EstablishedConnection) -> UpdateResult {
        log::debug!("Updating connection with request: {:?}", self);
        connection.send_disk_request(self);
        Ok(UpdateSuccess::Success)
    }
}

impl Request {
    pub fn new(block_index: usize, piece_index: usize, piece_info: PieceInfo) -> Self {
        Request {
            index: piece_index as _,
            begin: block_index as u32 * BLOCK_LENGTH as u32,
            length: piece_info.get_block_length(block_index as _, piece_index as _) as _,
        }
    }

    pub fn block_index(&self) -> usize {
        self.begin as usize / BLOCK_LENGTH
    }
    pub fn piece_index(&self) -> usize {
        self.index as _
    }

    pub fn offset(&self) -> usize {
        self.get_begin()
    }

    pub fn requested_piece_length(&self) -> usize {
        self.get_length()
    }
}

impl ProtocolMessage for Block {
    fn update(self, _: &mut EstablishedConnection) -> UpdateResult {
        panic!("read_and_update used instead");
    }
}

impl Block {
    pub fn new(request: &Request, block: Vec<u8>) -> Self {
        assert_eq!(request.requested_piece_length(), block.len());
        Block {
            index: request.index,
            begin: request.begin,
            block,
        }
    }

    pub fn read_and_update<T: Read>(
        reader: &mut T,
        block_manager: &mut BlockManager,
        length: usize,
    ) -> UpdateResult {
        // Combining read and update allows us to send the data straight where it needs to go instead
        // of copying it into the buffer and then copying it to the store.
        let (index, length) = u32::read_from(reader, length)?;
        let (begin, length) = u32::read_from(reader, length)?;
        log::debug!(
            "Reading and updating from Block message, index: {}, begin: {}, size: {}",
            index,
            begin,
            length,
        );
        block_manager.add_block(BlockReader::new(reader, begin as _, index as _, length))?;
        Ok(UpdateSuccess::Success)
    }

    pub fn read_and_update_utp<T: Read>(
        reader: &mut T,
        block_manager: &mut BlockManager,
        index: usize,
        begin: usize,
        length: usize,
    ) -> UpdateResult {
        // Combining read and update allows us to send the data straight where it needs to go instead
        // of copying it into the buffer and then copying it to the store.
        log::debug!(
            "Reading and updating from Block message, index: {}, begin: {}, size: {}",
            index,
            begin,
            length,
        );
        block_manager.add_block(BlockReader::new(reader, begin as _, index as _, length))?;
        Ok(UpdateSuccess::Success)
    }
}

impl ProtocolMessage for Cancel {
    fn update(self, connection: &mut EstablishedConnection) -> UpdateResult {
        connection.pending_peer_cancels.push(self);
        Ok(UpdateSuccess::Success)
    }
}

impl ProtocolMessage for Port {
    fn update(self, _connection: &mut EstablishedConnection) -> UpdateResult {
        // Simple ack, DHT not implemented
        Ok(UpdateSuccess::Success)
    }
}
