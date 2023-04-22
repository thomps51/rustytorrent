use std::io::Error;
use std::io::Read;
use std::io::Write;

use crate::common::Sha1Hash;
use crate::common::SHA1_HASH_LENGTH;
use log::{debug, info};
use write_to::WriteTo;

use super::read_byte;

type PeerId = [u8; 20];

#[derive(Debug, Clone, Default)]
pub struct Handshake {
    pub reserved: [u8; 8],
    pub info_hash: Sha1Hash,
    pub peer_id: PeerId,
}

impl Handshake {
    pub const PSTR: &'static [u8] = "BitTorrent protocol".as_bytes();
    pub const SIZE: u8 = 49 + Self::PSTR.len() as u8;

    pub fn new(peer_id: [u8; 20], info_hash: &Sha1Hash) -> Self {
        Self {
            reserved: [0, 0, 0, 0, 0, 0, 0, 0],
            info_hash: *info_hash,
            peer_id,
        }
    }

    pub fn read_from<T: Read>(reader: &mut T) -> Result<Self, Error> {
        debug!("Reading handshake");
        verify_pstr(reader)?;
        debug!("Verified PSTR");
        let mut info_hash = [0; SHA1_HASH_LENGTH];
        let mut peer_id = [0; SHA1_HASH_LENGTH];
        let mut reserved = [0; 8];
        reader.read_exact(&mut reserved)?;
        reader.read_exact(&mut info_hash)?;
        reader.read_exact(&mut peer_id)?;
        Ok(Handshake {
            reserved,
            info_hash,
            peer_id,
        })
    }
}

impl WriteTo for Handshake {
    fn write_to<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&[Self::PSTR.len() as u8])?;
        writer.write_all(Self::PSTR)?;
        writer.write_all(&self.reserved)?;
        writer.write_all(&self.info_hash)?;
        writer.write_all(&self.peer_id)?;
        Ok(())
    }
}

fn verify_pstr<T: Read>(reader: &mut T) -> Result<(), Error> {
    let size = read_byte(reader)?;
    if size as usize != Handshake::PSTR.len() {
        info!(
            "Peer failed handshake: expected {} for PSTR size, got {}",
            Handshake::PSTR.len(),
            size,
        );
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut buffer = [0; Handshake::PSTR.len()];
    reader.read_exact(&mut buffer)?;
    if buffer != Handshake::PSTR {
        info!(
            "Peer failed handshake: expected \"{:?}\" for PSTR, got \"{:?}\"",
            Handshake::PSTR,
            &buffer,
        );
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    Ok(())
}
