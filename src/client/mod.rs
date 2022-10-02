pub mod connection_manager;
pub mod controller;
pub mod disk_manager;

pub mod connection;
use std::io::{Read, Write};

pub use connection::*;

pub mod handshaking_connection;
pub use handshaking_connection::*;

pub mod established_connection;
pub use established_connection::*;

pub mod block_manager;
pub use block_manager::*;
use mio::{event::Source, net::TcpStream};
use std::fmt::Debug;

pub mod piece_info;

pub mod piece_assigner;

pub mod block_cache;

pub mod tracker_manager;

pub trait NetworkSourceType: Source + Send + Write + Read + Debug {
    fn peer_addr(&self) -> std::io::Result<std::net::SocketAddr>;
}
// impl<T> NetworkSourceType for T where T: Source + Send + Write + Read + Debug {}

impl NetworkSourceType for TcpStream {
    fn peer_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.peer_addr()
    }
}

pub type NetworkSource = Box<dyn NetworkSourceType>;
