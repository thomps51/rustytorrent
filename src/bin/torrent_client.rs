// Goal: CLI that, when given a torrent file, will download it to completion

use std::error::Error;
use std::path::Path;

use log::info;
use slog::Drain;

use rustytorrent::client::{ConnectionManager, ConnectionManagerConfig};
use rustytorrent::common::Torrent;
use rustytorrent::tracker::TrackerClientImpl;

#[macro_use(slog_o)]
extern crate slog;

fn main() -> Result<(), Box<dyn Error>> {
    let log_path = "rusty.log";
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path)
        .unwrap();

    let decorator = slog_term::PlainDecorator::new(file);
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();
    let logger = slog::Logger::root(drain, slog_o!());
    let _scope_guard = slog_scope::set_global_logger(logger);
    let _log_guard = slog_stdlog::init().unwrap();

    let args: Vec<String> = std::env::args().collect();
    // TODO validate args
    info!("Torrent file: {}", args[1]);
    let torrent = Torrent::from_file(Path::new(&args[1]), Path::new(""))?;
    let config = ConnectionManagerConfig {
        listen_port: 6800,
        max_peers: 50,
        seed: false,
        print_output: true,
    };
    let tracker = TrackerClientImpl {
        address: torrent.metainfo.announce.clone(),
        listen_port: config.listen_port,
    };
    let mut manager = ConnectionManager::new(config);
    manager.add_torrent(torrent, tracker, None)?;
    manager.run()?;
    info!("done!");
    Ok(())
}
