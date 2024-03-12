#![feature(duration_constants)]

use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    num::ParseIntError,
    time::{SystemTime, UNIX_EPOCH},
};

use rand::{distributions::Alphanumeric, Rng};
use rustytorrent::{
    client::utp::{Header, Type},
    common::{Sha1Hash, PEER_ID_LENGTH, PEER_ID_PREFIX},
    messages::Handshake,
};
use write_to::{ReadFrom, WriteTo};

pub fn new_udp_socket() -> UdpSocket {
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
    UdpSocket::bind(addr).unwrap()
}

fn send_header(socket: &UdpSocket, header: Header, addr: SocketAddr) {
    let mut send_buffer = [0u8; Header::SIZE];
    header.write_to(&mut send_buffer.as_mut_slice()).unwrap();
    println!("syn header: {header:?}");
    println!("syn raw bytes: {send_buffer:?}");
    socket.send_to(&send_buffer, addr).unwrap();
}

fn get_peer_id() -> [u8; PEER_ID_LENGTH] {
    let peer_id_suffix = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(PEER_ID_LENGTH - PEER_ID_PREFIX.len())
        .map(char::from);
    let mut peer_id = PEER_ID_PREFIX.to_owned();
    peer_id.extend(peer_id_suffix);
    let peer_id: [u8; PEER_ID_LENGTH] = peer_id.into_bytes().try_into().unwrap();
    peer_id
}

pub fn decode_hex(s: &str) -> Result<Vec<u8>, ParseIntError> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
        .collect()
}

fn main() {
    let info_hash_hex = decode_hex("67721401435f94abb89449bc029355e2c32083f3").unwrap();
    let info_hash: Sha1Hash = [
        103, 114, 20, 1, 67, 95, 148, 171, 184, 148, 73, 188, 2, 147, 85, 226, 195, 32, 131, 243,
    ];
    if info_hash_hex != info_hash {
        panic!("incorrect info_hash!");
    }
    let peer_id = get_peer_id();
    let mut recv_buffer = Vec::new();
    let socket = new_udp_socket();
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 1980));
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let ms = now.as_micros();
    let syn_header = Header::new(Type::StSyn, 1227, ms as _, 0, 10000, 10143, 0);
    send_header(&socket, syn_header, addr);
    recv_buffer.resize(1000, 0);
    let (size, dst_addr) = socket.recv_from(&mut recv_buffer).unwrap();
    recv_buffer.truncate(size);
    let (state_header, _) = Header::read_from(&mut recv_buffer.as_slice(), size).unwrap();

    let mut send_buffer = Vec::new();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let ms = now.as_micros();
    Header::new(
        Type::StData,
        1228,
        ms as _,
        32172291,
        100000,
        10144,
        state_header.seq_nr - 1,
    )
    .write_to(&mut send_buffer)
    .unwrap();
    let handshake = Handshake::new(peer_id, &info_hash);
    handshake.write_to(&mut send_buffer).unwrap();
    println!(
        "Sending message len: {}, Handshake: {:?}",
        send_buffer.len(),
        handshake
    );
    println!("Raw handshake: {:?}", &send_buffer[Header::SIZE..]);
    socket.send_to(&send_buffer, dst_addr).unwrap();

    loop {
        recv_buffer.resize(1000, 0);
        let (size, dst_addr) = socket.recv_from(&mut recv_buffer).unwrap();
        let (recv_header, size) = Header::read_from(&mut recv_buffer.as_slice(), size).unwrap();
        println!(
            "Recieved header of type {:?}, size: {}, addr: {}",
            recv_header.get_type(),
            size,
            dst_addr
        );
        if recv_header.get_type() == Type::StFin {
            break;
        }
    }
}
