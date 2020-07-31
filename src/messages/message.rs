use super::super::Connection;
use super::super::UpdateResult;
use std::io::Error;
use std::io::Read;
use std::io::Write;

pub enum MessageLength {
    Fixed(u32),
    Variable,
}

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
    // implementations should not read the id in ReadFrom.
    fn read_data<T: Read>(reader: &mut T, length: u32) -> Result<Self, Error>;

    fn read_from<T: Read>(reader: &mut T, length: u32) -> Result<Self, Error> {
        if let MessageLength::Fixed(expected) = Self::SIZE {
            if expected != length {
                panic!("Fixed-sized message received with wrong size");
            }
        }
        let message = Self::read_data(reader, length)?;
        //println!("Read message of type: {}", Self::NAME);
        Ok(message)
    }

    fn update(self, connection: &mut Connection) -> UpdateResult;

    fn write_data<T: Write>(&self, _: &mut T) -> Result<(), Error> {
        debug_assert_eq!(std::mem::size_of::<Self>(), 0);
        Ok(())
    }

    // Unlike ReadFrom, we don't know which particular message we have when we want
    fn write_to<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        //println!("Writing message: {:?}", self);
        writer.write_all(&self.length_be_bytes())?;
        writer.write_all(&[Self::ID as u8])?;
        self.write_data(writer)?;
        Ok(())
    }
}
