pub mod constants;
pub use constants::*;

pub mod hash;
pub use hash::*;

pub mod meta_info;
pub use meta_info::*;

pub mod torrent;
pub use torrent::*;

pub mod create_torrent;

use std::rc::Rc;

use crate::client::{block_cache::BlockCache, piece_assigner::PieceAssigner};

use std::cell::RefCell;
pub type SharedPieceAssigner = Rc<RefCell<PieceAssigner>>;
pub type SharedBlockCache = Rc<RefCell<BlockCache>>;
pub type SharedCount = Rc<RefCell<usize>>;
