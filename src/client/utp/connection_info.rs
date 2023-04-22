use std::{
    net::SocketAddr,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{Header, Type};
use rand::Rng;

#[derive(Debug)]
pub struct UtpConnectionInfo {
    addr: SocketAddr,
    pub seq_nr: u16,
    pub ack_nr: u16,
    pub conn_id_recv: u16,
    conn_id_send: u16,
    wnd_size: u32,
    prev_timestamp_diff: u32,
}

impl UtpConnectionInfo {
    pub fn new(addr: SocketAddr) -> Self {
        let conn_id_recv = rand::thread_rng().gen();
        Self {
            seq_nr: 1,
            conn_id_recv,
            conn_id_send: conn_id_recv + 1,
            ack_nr: 0,
            wnd_size: 10000, // TODO: No idea what this should be
            prev_timestamp_diff: 0,
            addr,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn new_from_incoming(addr: SocketAddr, header: &Header) -> Self {
        Self {
            seq_nr: 1,
            conn_id_recv: header.connection_id + 1,
            conn_id_send: header.connection_id, // TODO: Why is this not +1?  It seems to want me to send back to the same id
            ack_nr: 0,
            wnd_size: 10000, // TODO: No idea what this should be
            prev_timestamp_diff: 0,
            addr,
        }
    }

    pub fn create_header(&mut self, header_type: Type) -> Header {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let ms = now.as_micros();
        let conn_id = if Type::StSyn == header_type {
            self.conn_id_recv
        } else {
            self.conn_id_send
        };
        let msg = Header::new(
            header_type,
            conn_id,
            ms as _,
            self.prev_timestamp_diff,
            self.wnd_size,
            self.seq_nr,
            self.ack_nr,
        );
        if header_type != Type::StState {
            self.seq_nr += 1;
        }
        msg
    }

    pub fn process_header(&mut self, header: &Header) {
        self.ack_nr = header.seq_nr as _;
        // self.conn_id_send = header.connection_id;
        // self.wnd_size = header.wnd_size;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as u32;
        let (prev_timestamp_diff, _) = now.overflowing_sub(header.timestamp_microseconds);
        self.prev_timestamp_diff = prev_timestamp_diff;
    }

    pub fn packet_size(&self) -> usize {
        // TODO: actually implement utp to determine this
        1000
    }
}
