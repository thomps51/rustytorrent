use log::{debug, info};
use rand::Rng;
use slog::Drain;
use slog_scope::GlobalLoggerGuard;
use std::cmp;
use std::io::BufWriter;
use std::io::Write;
use tempfile::NamedTempFile;

use slog::slog_o;
use std::sync::Once;

/// Setup function that is only run once, even if called multiple times.
fn setup() -> GlobalLoggerGuard {
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
    _scope_guard
}

fn setup_env() {
    env_logger::builder()
        .format(|buf, record| {
            let ts = buf.timestamp_micros();
            writeln!(
                buf,
                "{}: {:?}: {}: {}",
                ts,
                std::thread::current().id(),
                buf.default_level_style(record.level())
                    .value(record.level()),
                record.args()
            )
        })
        .init();
}

fn create_random_file(size: usize) -> NamedTempFile {
    let file = NamedTempFile::new().unwrap();
    let mut writer = BufWriter::new(file);

    let mut rng = rand::thread_rng();
    let mut buffer = [0; 1024];
    let mut remaining_size = size;

    while remaining_size > 0 {
        let to_write = cmp::min(remaining_size, buffer.len());
        let buffer = &mut buffer[..to_write];
        rng.fill(buffer);
        writer.write(buffer).unwrap();

        remaining_size -= to_write;
    }
    writer.into_inner().unwrap()
}

// #[test]
// fn seeder_only() {
//     use crate::{
//         client::{ConnectionManager, ConnectionManagerConfig},
//         common::{create_torrent::create_torrent_metadata_from_path, Torrent},
//         tracker::TestTrackerClient,
//     };
//     use num_traits::pow;

//     let piece_length = pow(2, 18); // 256 K
//     let file = create_random_file(10_000_001);
//     let path = file.path().to_owned();
//     println!("Created random file at {:?}", path);
//     // let contents = std::fs::read(&path).unwrap();
//     // println!("file contents: {:?}", contents);

//     let torrent_metadata = create_torrent_metadata_from_path(&path, "TEST", piece_length).unwrap();
//     let torrent =
//         Torrent::from_dictionary(torrent_metadata.clone(), path.parent().unwrap()).unwrap();

//     let mut seeder = ConnectionManager::new(ConnectionManagerConfig {
//         listen_port: 6800,
//         max_peers: 50,
//         seed: true,
//         print_output: false,
//     });
//     seeder
//         .add_torrent(torrent, TestTrackerClient::new_empty(), None)
//         .unwrap();
//     seeder.start().unwrap();
//     println!("Seeder started");
//     loop {
//         seeder.poll().unwrap();
//     }
// }

#[test]
fn one_seeder_one_downloader() {
    let _lg = setup_env();
    use crate::{
        client::{ConnectionManager, ConnectionManagerConfig},
        common::{create_torrent::create_torrent_metadata_from_path, Torrent},
        tracker::TestTrackerClient,
    };
    use num_traits::pow;
    use std::sync::mpsc::channel;
    use std::thread;

    let piece_length = pow(2, 18); // 256 K
    let file = create_random_file(10_000_001);
    let path = file.path().to_owned();

    let torrent_metadata = create_torrent_metadata_from_path(&path, "TEST", piece_length).unwrap();
    let torrent =
        Torrent::from_dictionary(torrent_metadata.clone(), path.parent().unwrap()).unwrap();
    let torrent_downloader =
        Torrent::from_dictionary(torrent_metadata, &std::env::temp_dir().join("supertemp"))
            .unwrap();
    let (sender, recv) = channel();

    let (start_sender, start_recv) = channel();
    let (stop_sender, stop_recv) = channel();

    let s_thread = thread::spawn(move || {
        let mut seeder = ConnectionManager::new(ConnectionManagerConfig {
            listen_port: 6800,
            max_peers: 50,
            seed: true,
            print_output: false,
        });
        seeder
            .add_torrent(torrent, TestTrackerClient::new_empty(), None)
            .unwrap();
        seeder.start().unwrap();
        info!("Seeder started");
        start_sender.send(true).unwrap();
        loop {
            seeder.poll().unwrap();
            if let Ok(_) = recv.try_recv() {
                info!("Stopping seeder");
                break;
            }
        }
        stop_sender.send(true).unwrap();
    });
    let d_thread = thread::spawn(move || {
        start_recv.recv().unwrap();
        let mut downloader = ConnectionManager::new(ConnectionManagerConfig {
            listen_port: 6801,
            max_peers: 50,
            seed: false,
            print_output: false,
        });
        downloader
            .add_torrent(
                torrent_downloader,
                TestTrackerClient::new_local(6800),
                Some(sender),
            )
            .unwrap();
        downloader.start().unwrap();
        info!("Downloader started");
        loop {
            downloader.poll().unwrap();
            if let Ok(_) = stop_recv.try_recv() {
                info!("Stopping downloader");
                break;
            }
        }
    });
    s_thread.join().unwrap();
    d_thread.join().unwrap();
    info!("Simple test complete!")
}
