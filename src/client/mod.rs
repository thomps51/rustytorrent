pub mod block_manager;
use std::sync::mpsc::Sender;

pub use block_manager::*;

pub mod connection;
pub use connection::*;

pub mod handshaking_connection;
pub use handshaking_connection::*;

pub mod established_connection;
pub use established_connection::*;

pub mod connection_manager;
pub use connection_manager::*;

mod endgame;
// use endgame::*;

// TODO: make these not public? Piece assigner at least
pub mod piece_assigner;
pub use piece_assigner::*;
pub mod piece_store;
pub use piece_store::*;
pub mod piece_info;

// use endgame::*;

// pub mod connection;
// pub use connection::*;

// pub mod connection;
// pub use connection::*;

pub type CompletionHandler = Sender<bool>;
