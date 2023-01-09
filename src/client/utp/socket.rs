use std::{
    io,
    io::Write,
    net::SocketAddr,
    rc::Rc,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{Header, Type};
use mio::net::UdpSocket;
use rand::Rng;
use write_to::WriteTo;

use crate::messages::ProtocolMessage;

#[derive(Debug)]
pub struct UtpSocket {
    socket: Rc<UdpSocket>,
    addr: SocketAddr,
    pub seq_nr: u16,
    pub ack_nr: u16,
    conn_id_recv: u16,
    conn_id_send: u16,
    wnd_size: u32,
    send_buffer: Vec<u8>,
    prev_timestamp_diff: u32,
}

// impl Read for UtpSocket {
//     fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
//         // Don't read here, we should only read from ConnectionManager to determine which connection.
//         panic!("Don't Read UTP Socket!");

//         // // Read, stripping headers
//         // let read = self.socket.recv(&mut self.recv_buffer)?;
//         // let (header, remaining) =
//         //     Header::read_from(&mut std::io::Cursor::new(&self.recv_buffer), read)?;
//         // unsafe {
//         //     let dst_ptr = buf.as_mut_ptr();
//         //     let src_ptr = self.recv_buffer.as_ptr().offset(Header::SIZE as _);
//         //     std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, remaining);
//         // }
//         // self.ack_nr = header.seq_nr as _;
//         // let now = SystemTime::now()
//         //     .duration_since(UNIX_EPOCH)
//         //     .unwrap()
//         //     .as_micros() as u32;
//         // self.prev_timestamp_diff = now - header.timestamp_microseconds;
//         // Ok(remaining)
//         // Maybe try to use recvmmsg (recvmsg_x on mac) to get multiple udp packets at once
//         // Or scatter-gather so we can read Header into stack buffer here
//         // and rest into buffer.
//         // let header_buffer = [0; Header::SIZE];
//     }
// }

impl Write for UtpSocket {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        log::debug!("UtpSocket::Write");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let ms = now.as_micros();
        let header = Header::new(
            Type::StData,
            self.conn_id_send,
            ms as _,
            self.prev_timestamp_diff,
            self.wnd_size,
            self.seq_nr,
            self.ack_nr,
        );
        log::debug!("write_buf header: {:?}", header);
        self.seq_nr += 1;
        self.send_buffer.clear();
        header.write_to(&mut self.send_buffer)?;
        self.send_buffer.write_all(buf)?;
        let sent = self.socket.send_to(&self.send_buffer, self.addr)?;
        self.send_buffer.clear();
        Ok(sent.saturating_sub(20))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl UtpSocket {
    pub fn new(socket: Rc<UdpSocket>, addr: SocketAddr) -> Self {
        let conn_id_recv = rand::thread_rng().gen();
        Self {
            socket,
            seq_nr: 1,
            conn_id_recv,
            conn_id_send: conn_id_recv + 1,
            ack_nr: 0,
            send_buffer: Vec::new(),
            wnd_size: 10000, // TODO: No idea what this should be
            prev_timestamp_diff: 0,
            addr,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn new_from_incoming(socket: Rc<UdpSocket>, addr: SocketAddr, header: &Header) -> Self {
        Self {
            socket,
            seq_nr: 1,
            conn_id_recv: header.connection_id + 1,
            conn_id_send: header.connection_id, // TODO: Why is this not +1?  It seems to want me to send back to the same id
            ack_nr: 0,
            send_buffer: Vec::new(),
            wnd_size: 10000, // TODO: No idea what this should be
            prev_timestamp_diff: 0,
            addr,
        }
    }

    fn create_header(&mut self, header_type: Type) -> Header {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let ms = now.as_micros();
        let msg = Header::new(
            header_type,
            self.conn_id_send,
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

    pub fn send_header(&mut self, header_type: Type) -> io::Result<()> {
        let msg = self.create_header(header_type);
        log::debug!(
            "sending header: {:?} to {:?}: {:?}",
            header_type,
            self.addr,
            msg
        );
        self.send_buffer.clear();
        msg.write_to(&mut self.send_buffer)?;
        self.socket.send_to(&self.send_buffer, self.addr)?;
        self.send_buffer.clear();
        Ok(())
    }

    pub fn send_syn(&mut self) -> io::Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let ms = now.as_micros();
        let msg = Header::new(
            Type::StSyn,
            self.conn_id_recv,
            ms as _, // 2032 bug, but that's the protocol
            0,
            10000,
            self.seq_nr,
            0,
        );
        self.seq_nr += 1;
        self.send_buffer.clear();
        msg.write_to(&mut self.send_buffer)?;
        log::debug!(
            "Sending header of size {} to {:?}: {:?}",
            self.send_buffer.len(),
            self.addr,
            msg
        );
        self.socket.send_to(&self.send_buffer, self.addr)?;
        self.send_buffer.clear();
        Ok(())
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

    pub fn write_buf(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let ms = now.as_micros();
        let header = Header::new(
            Type::StData,
            self.conn_id_send,
            ms as _,
            self.prev_timestamp_diff,
            self.wnd_size,
            self.seq_nr,
            self.ack_nr,
        );
        log::debug!(
            "write_buf sending {} bytes with header: {:?}",
            buf.len(),
            header
        );
        self.seq_nr += 1;
        self.send_buffer.clear();
        header.write_to(&mut self.send_buffer)?;
        self.send_buffer.write_all(buf)?;
        let sent = self.socket.send_to(&self.send_buffer, self.addr)?;
        self.send_buffer.clear();
        Ok(sent)
    }

    pub fn write<T: ProtocolMessage>(&mut self, msg: &T) -> std::io::Result<usize> {
        log::debug!("UtpSocket::Write");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let ms = now.as_micros();
        let header = Header::new(
            Type::StData,
            self.conn_id_send,
            ms as _,
            self.prev_timestamp_diff,
            self.wnd_size,
            self.seq_nr,
            self.ack_nr,
        );
        self.seq_nr += 1;
        self.send_buffer.clear();
        header.write_to(&mut self.send_buffer)?;
        msg.write(&mut self.send_buffer)?;
        log::debug!("UTP sending message of len {:?}", self.send_buffer.len());
        let sent = self.socket.send_to(&self.send_buffer, self.addr)?;
        self.send_buffer.clear();
        Ok(sent)
    }

    // pub fn recv_syn_response(&mut self, recv_buffer: &mut ReadBuffer) -> io::Result<bool> {
    //     assert_eq!(0, recv_buffer.unread());
    //     assert!(20 <= recv_buffer.unused());
    //     let buffer = recv_buffer.get_unused_mut();
    //     let read = match self.socket.recv(buffer) {
    //         Ok(read) => read,
    //         Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
    //         Err(error) => return Err(error),
    //     };
    //     recv_buffer.added_unused(read);
    //     if read != Header::SIZE {
    //         return Err(io::ErrorKind::InvalidData.into());
    //     }
    //     let (header, _) = Header::read_from(recv_buffer, read)?;
    //     self.process_header(&header);
    //     Ok(true)
    // }

    // pub fn recv_header(&mut self, recv_buffer: &mut ReadBuffer) -> io::Result<Option<Type>> {
    //     assert_eq!(0, recv_buffer.unread());
    //     assert!(20 <= recv_buffer.unused());
    //     let buffer = recv_buffer.get_unused_mut();
    //     let read = match self.socket.recv(buffer) {
    //         Ok(read) => read,
    //         Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
    //         Err(error) => return Err(error),
    //     };
    //     recv_buffer.added_unused(read);
    //     if read != Header::SIZE {
    //         log::warn!("Received utp header of unexpected size: {:?}", read);
    //         return Err(io::ErrorKind::InvalidData.into());
    //     }
    //     let (header, _) = Header::read_from(recv_buffer, read)?;
    //     self.process_header(&header);
    //     Ok(Some(header.get_type()))
    // }

    // pub fn send<T: WriteTo>(&mut self, message: &T) -> io::Result<()> {
    //     let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    //     let ms = now.as_micros();
    //     let header = Header {
    //         type_ver_extension: (1 >> 4) | Type::ST_SYN as u16,
    //         connection_id: self.conn_id_send,
    //         timestamp_microseconds: ms as _,
    //         timestamp_difference_microseconds: self.prev_timestamp_diff,
    //         wnd_size: self.wnd_size,
    //         seq_nr: self.seq_nr,
    //         ack_nr: self.ack_nr,
    //     };
    //     self.seq_nr += 1;
    //     self.send_buffer.clear();
    //     header.write_to(&mut self.send_buffer)?;
    //     message.write_to(&mut self.send_buffer)?;
    //     let _sent = self.socket.send(&self.send_buffer)?;
    //     self.send_buffer.clear();
    //     Ok(())
    // }
}
