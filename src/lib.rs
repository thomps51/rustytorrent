// #![feature(adt_const_params)]
// #![feature(generic_const_exprs)]
#![feature(try_blocks)]
#![feature(let_chains)]
#![feature(read_buf)]
#![allow(clippy::new_without_default)]
pub mod bencoding;
// pub use bencoding::*;

// pub mod old_client;
// pub use client::*;

pub mod common;
// pub use common::*;

pub mod io;
// pub use io::*;

pub mod messages;
// pub use messages::*;

pub mod tracker;
// pub use tracker::*;

mod tests;

pub mod client;
