use std::io::Error;
use std::io::Read;
use std::io::Write;

use super::super::Connection;
use super::super::UpdateResult;
use super::super::UpdateSuccess;

type PeerId = [u8; 20];

#[derive(Debug, Clone, Default)]
pub struct Handshake {
    pub info_hash: crate::hash::Sha1Hash,
    pub peer_id: PeerId,
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

    pub fn write_to<T: Write>(&self, writer: &mut T) -> Result<(), Error> {
        writer.write_all(&[Self::PSTR.len() as u8])?;
        writer.write_all(Self::PSTR)?;
        writer.write_all(&[0, 0, 0, 0, 0, 0, 0, 0])?;
        writer.write_all(&self.info_hash)?;
        writer.write_all(&self.peer_id)?;
        /*
        let mut buffer = Vec::new();
        buffer.push(Self::PSTR.len() as u8);
        buffer.extend_from_slice(Self::PSTR);
        buffer.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        buffer.extend_from_slice(&self.info_hash);
        buffer.extend_from_slice(&self.peer_id);
        debug_assert_eq!(buffer.len() as u8, Self::SIZE);
        println!("Sending handshake {:?}", buffer);
        writer.write_all(&buffer)?;
        */
        Ok(())
    }
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
