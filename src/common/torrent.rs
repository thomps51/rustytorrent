use crate::bencoding::Dictionary;
use crate::common::MetaInfo;
use std::error::Error;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Torrent {
    pub metainfo: MetaInfo,
    pub downloaded: usize,
    pub uploaded: usize,
    pub left: usize,
    pub destination: PathBuf,
}

impl Torrent {
    pub fn from_file(file: &Path, destination: &Path) -> Result<Torrent, Box<dyn Error>> {
        log::debug!("From file");
        let metainfo = MetaInfo::from_file(file)?;
        // log::debug!("got metainfo: {:?}", metainfo);
        let total_size = metainfo.total_size;
        Ok(Torrent {
            metainfo,
            downloaded: 0,
            uploaded: 0,
            left: total_size,
            destination: destination.to_owned(),
        })
    }

    pub fn from_dictionary(
        info: Dictionary,
        destination: &Path,
    ) -> Result<Torrent, Box<dyn Error>> {
        let metainfo = MetaInfo::from_dict(info)?;
        let total_size = metainfo.total_size;
        Ok(Torrent {
            metainfo,
            downloaded: 0,
            uploaded: 0,
            left: total_size,
            destination: destination.to_owned(),
        })
    }
}
