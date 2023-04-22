use std::io::{Read, Result, Write};

use log::{debug, info};
use write_to::{Length, Name, ReadFrom, WriteTo};

pub trait HasId {
    const ID: u8;
}

pub trait ProtocolMessage: Sized + Name + Length + ReadFrom + WriteTo + HasId {
    // When reading the message, we already know what we will be reading thanks to the id.  Therefore,
    // implementations should not read the id in ReadFrom.
    fn read<T: Read>(reader: &mut T, length: usize) -> Result<Self> {
        let (message, remaining) = Self::read_from(reader, length)?;
        if remaining != 0 {
            info!(
                "Message {} received with wrong size, had bytes remaining. Got: {}",
                Self::NAME,
                length
            );
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        debug!("Read message of type: {}", Self::NAME);
        Ok(message)
    }

    // Unlike ReadFrom, we don't know which particular message we have when we want
    fn write<T: Write>(&self, writer: &mut T) -> Result<()> {
        // debug!("Writing message of type: {}", Self::NAME);
        let length = self.length() as u32 + 1; // Add ID byte
        writer.write_all(&length.to_be_bytes())?;
        writer.write_all(&[Self::ID])?;
        self.write_to(writer)?;
        Ok(())
    }
}
