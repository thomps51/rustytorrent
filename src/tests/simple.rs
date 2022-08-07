#[cfg(test)]
mod tests {

    use log::info;
    use rand::Rng;
    use std::cmp;
    use std::io::BufWriter;
    use std::io::Write;
    use tempfile::NamedTempFile;

    use crate::client::controller::Controller;
    use crate::client::controller::ControllerConfig;
    use crate::client::controller::ControllerInputMessage;

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

    #[cfg(test)]
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

    #[test]
    fn one_seeder_one_downloader() {
        let _lg = setup_env();
        use crate::common::{create_torrent::create_torrent_metadata_from_path, Torrent};
        use num_traits::pow;
        use std::sync::mpsc::channel;
        use std::thread;

        let piece_length = pow(2, 18); // 256 KiB
        let file = create_random_file(10_000_001);
        let path = file.path().to_owned();

        let torrent_metadata =
            create_torrent_metadata_from_path(&path, "TEST", piece_length).unwrap();
        let mut torrent =
            Torrent::from_dictionary(torrent_metadata.clone(), path.parent().unwrap()).unwrap();
        let mut torrent_downloader =
            Torrent::from_dictionary(torrent_metadata, &std::env::temp_dir().join("supertemp"))
                .unwrap();
        let (sender, recv) = channel();

        // Start up the downloaded, then start up the seeder (which will take longer),
        // have seeder connect to downloader via dummy tracker
        //
        // To stop, register a completion handler that will stop the downloader and seeder.

        let (s_sender, s_recv) = channel();
        let s_sender_two = s_sender.clone();
        let (d_sender, d_recv) = channel();
        let d_sender_two = d_sender.clone();

        let s_thread = thread::spawn(move || {
            let mut seeder = Controller::new_from_channel(
                ControllerConfig {
                    listen_port: 6800,
                    max_peers: 50,
                    seed: true,
                    print_output: false,
                },
                s_sender,
                s_recv,
            );
            torrent.metainfo.announce = "TestTrackerSelf:6801".into();
            seeder.add_torrent(torrent, None);
            info!("Seeder started");
            seeder.run();
        });
        let d_thread = thread::spawn(move || {
            let mut downloader = Controller::new_from_channel(
                ControllerConfig {
                    listen_port: 6801,
                    max_peers: 50,
                    seed: false,
                    print_output: false,
                },
                d_sender,
                d_recv,
            );
            torrent_downloader.metainfo.announce = "None".into();
            downloader.add_torrent(torrent_downloader, Some(sender));
            info!("Downloader started");
            downloader.run();
        });
        let _ = recv.recv().unwrap();
        d_sender_two.send(ControllerInputMessage::Stop).unwrap();
        s_sender_two.send(ControllerInputMessage::Stop).unwrap();

        s_thread.join().unwrap();
        d_thread.join().unwrap();
        info!("Simple test complete!")
    }
}
