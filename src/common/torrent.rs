use crate::common::MetaInfo;
use std::error::Error;
use std::path::Path;

pub struct Torrent {
    pub metainfo: MetaInfo,
    pub downloaded: usize,
    pub uploaded: usize,
    pub left: usize,
}

impl Torrent {
    pub fn from_file(file: &Path) -> Result<Torrent, Box<dyn Error>> {
        let metainfo = MetaInfo::from_file(file)?;
        let total_size = metainfo.total_size;
        Ok(Torrent {
            metainfo: metainfo,
            downloaded: 0,
            uploaded: 0,
            left: total_size,
        })
    }
}
