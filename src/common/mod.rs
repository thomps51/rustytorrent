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

use crate::client::FileSystem;
use crate::client::PieceAssigner;

use std::cell::RefCell;
pub type SharedPieceAssigner = Rc<RefCell<PieceAssigner>>;
pub type SharedPieceStore = Rc<RefCell<FileSystem>>; // impl Trait syntax pls
