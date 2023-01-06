#![feature(try_blocks)]

use std::io::Cursor;
use std::{error::Error, net::Ipv4Addr};

use log::warn;
use std::net::UdpSocket;
// use mio::{net::UdpSocket, Events, Interest, Poll, Token};
use rustytorrent::{
    messages::udp_tracker::{
        AnnounceRequest, AnnounceResponse, ConnectRequest, ConnectResponse, Endpoint,
    },
    tracker::upd_tracker::Action,
};
use write_to::{ReadFrom, WriteTo};

// struct Tracker {
//     next_connection_id: usize,
// }

fn handle_event(socket: &UdpSocket, buffer: &mut Vec<u8>) -> std::io::Result<()> {
    match socket.recv_from(buffer) {
        Ok((length, addr)) => {
            buffer.resize(length, 0);

            match length {
                16 => {
                    let (msg, _) = ConnectRequest::read_from(&mut Cursor::new(&buffer), length)?;
                    println!("Received {addr:?}: {msg:?}");
                    let connection_id = if addr.port() == 7000 {
                        println!("our client!");
                        // This is our client
                        0
                    } else {
                        // Other client
                        1
                    };
                    let resp = ConnectResponse {
                        action: Action::Connect as _,
                        transaction_id: msg.transaction_id,
                        connection_id,
                    };
                    buffer.clear();
                    resp.write_to(buffer)?;
                    println!("Sending to {addr:?}: {buffer:?}");
                    socket.send_to(buffer, addr)?;
                    buffer.resize(65000, 0);
                }
                98 => {
                    let (msg, _) = AnnounceRequest::read_from(&mut Cursor::new(&buffer), length)?;
                    println!("Received AnnounceRequest: {msg:?}");

                    let seeders: i32;
                    let leechers: i32;
                    let peers = if msg.connection_id == 0 {
                        seeders = 1;
                        leechers = 0;
                        // This is our client
                        // First test will be we are downloading but we want them to connect to us, so return nothing.
                        vec![]
                    } else {
                        // Other client
                        // Return the other client
                        seeders = 0;
                        leechers = 1;
                        let ip = i32::from_le_bytes(Ipv4Addr::new(127, 0, 0, 1).octets());
                        vec![Endpoint { ip, port: 6801 }]
                    };

                    let resp = AnnounceResponse {
                        action: Action::Announce as _,
                        transaction_id: msg.transaction_id,
                        interval: 10 * 60,
                        leechers,
                        seeders,
                        peers,
                    };
                    buffer.clear();
                    println!("Sending to {addr:?}: {resp:?}");
                    resp.write_to(buffer)?;
                    socket.send_to(buffer, addr)?;
                    buffer.resize(65000, 0);
                }
                _ => {
                    println!("received message of length {length}");
                }
            }
            // match addr
        }
        Err(error) => {
            warn!("Error! {:?}", error);
            // break;
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("1");
    // let mut poll = Poll::new().unwrap();
    // let mut socket =
    // UdpSocket::bind(format!("127.0.0.1:{}", 6900).as_str().parse().unwrap()).unwrap();
    let socket = UdpSocket::bind("127.0.0.1:6900").expect("couldn't bind to address");

    // let token = Token(0);
    // poll.registry()
    //     .register(&mut socket, token, Interest::READABLE)
    //     .unwrap();
    // let mut events = Events::with_capacity(10);
    let mut buffer = Vec::with_capacity(65000);
    buffer.resize(65000, 0);

    loop {
        println!("Polling");
        // poll.poll(&mut events, None).unwrap();
        loop {
            handle_event(&socket, &mut buffer).unwrap();
        }
    }
    // Ok(())
}
