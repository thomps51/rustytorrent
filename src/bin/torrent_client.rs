// Goal: CLI that, when given a torrent file, will download it to completion

use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::sync::mpsc::channel;
use std::thread;

use log::{debug, info};
use rustytorrent::client::controller::{Controller, ControllerConfig, ControllerInputMessage};
// use slog::Drain;

use rustytorrent::common::Torrent;

// #[macro_use(slog_o)]
// extern crate slog;

fn setup_env() {
    env_logger::builder()
        .format(|buf, record| {
            let ts = buf.timestamp_micros();
            writeln!(
                buf,
                "{}: {:?}: {}:{}: {}: {}",
                ts,
                std::thread::current().id(),
                record.file().unwrap_or("unknown"),
                record.line().unwrap_or(0),
                buf.default_level_style(record.level())
                    .value(record.level()),
                record.args()
            )
        })
        .init();
}
fn main() -> Result<(), Box<dyn Error>> {
    // let log_path = "rusty.log";
    // let file = std::fs::OpenOptions::new()
    //     .create(true)
    //     .write(true)
    //     .truncate(true)
    //     .open(log_path)
    //     .unwrap();

    // let decorator = slog_term::PlainDecorator::new(file);
    // let drain = slog_term::FullFormat::new(decorator).build().fuse();
    // let drain = slog_async::Async::new(drain).build().fuse();
    // let logger = slog::Logger::root(drain, slog_o!());
    // let _scope_guard = slog_scope::set_global_logger(logger);
    // let _log_guard = slog_stdlog::init().unwrap();
    setup_env();

    let args: Vec<String> = std::env::args().collect();
    // TODO validate args
    info!("Torrent file: {}", args[1]);
    debug!("test debug");
    let torrent = Torrent::from_file(Path::new(&args[1]), Path::new(""))?;
    debug!("Torrent created");
    let (sender, recv) = channel();
    let (d_sender, d_recv) = channel();
    let d_sender_two = d_sender.clone();
    let d_thread = thread::spawn(move || {
        let mut downloader = Controller::new_from_channel(
            ControllerConfig {
                listen_port: 6800,
                max_peers: 100,
                seed: false,
                print_output: true,
            },
            d_sender,
            d_recv,
        );
        downloader.add_torrent(torrent, Some(sender));
        info!("Downloader started");
        downloader.run();
    });
    debug!("Created contoller");
    info!("done!");
    let _ = recv.recv().unwrap();
    d_sender_two.send(ControllerInputMessage::Stop).unwrap();
    d_thread.join().unwrap();
    Ok(())
}
