use std::{io::Write, net::SocketAddr};

use mio::net::UdpSocket;
use write_to::WriteTo;

use crate::{io::sendmmsg::UtpBlockSender, messages::ProtocolMessage};

use super::{Header, Type, UtpConnectionInfo};

#[derive(Default)]
pub struct UtpSendBuffer {
    data_buffer: Vec<u8>,
    header_only: Vec<Type>,
    header_buffer: [u8; Header::SIZE],
}

impl UtpSendBuffer {
    pub fn add_message<T: ProtocolMessage>(&mut self, message: T) {
        message
            .write(&mut self.data_buffer)
            .expect("vec write can't fail");
    }

    pub fn add_data<T: WriteTo>(&mut self, data: T) {
        data.write_to(&mut self.data_buffer)
            .expect("vec write can't fail");
    }

    pub fn add_header(&mut self, header: Type) {
        self.header_only.push(header);
    }

    pub fn send_to(
        &mut self,
        utp_sender: &mut UtpBlockSender,
        socket: &UdpSocket,
        conn_info: &mut UtpConnectionInfo,
    ) -> std::io::Result<()> {
        for header_type in self.header_only.drain(..) {
            let header = conn_info.create_header(header_type);
            header
                .write_to(&mut self.header_buffer.as_mut_slice())
                .unwrap();
            socket.send_to(&self.header_buffer, conn_info.addr())?;
        }
        if self.data_buffer.is_empty() {
            return Ok(());
        }
        let header = conn_info.create_header(Type::StData);
        // TODO: get packet size from conn_info
        let sent = utp_sender.send(
            header,
            socket,
            1000,
            &mut self.data_buffer,
            conn_info.addr(),
        )?;
        self.data_buffer.clear();
        // sent - 1 because create_header adds one (TODO: This smells)
        let value = conn_info.seq_nr.wrapping_add((sent - 1) as u16);
        conn_info.seq_nr = value;
        Ok(())
    }

    // Send the message to all peers in the iterator
    pub fn send_to_all(
        &mut self,
        utp_sender: &mut UtpBlockSender,
        connections: impl Iterator<Item = (SocketAddr, usize, Header)>,
        socket: &UdpSocket,
    ) -> std::io::Result<()> {
        let result = utp_sender.send_all(connections, &mut self.data_buffer, socket);
        self.data_buffer.clear();
        result
    }
}

impl Write for UtpSendBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.data_buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
