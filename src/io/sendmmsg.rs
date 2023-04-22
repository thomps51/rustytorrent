use std::net::SocketAddr;
use std::os::fd::AsRawFd;

use mio::net::UdpSocket;
use write_to::WriteTo;

use super::MsghdrX;
use crate::client::utp::Header;
use crate::common::BLOCK_LENGTH;
use crate::io::socketaddr_to_raw;
use crate::messages::Block;

// examples which helped implement this:
// https://gist.github.com/Maximus-/487c70508e161680d550ccb343781859
// https://github.com/grimm-co/NotQuite0DayFriday/blob/trunk/2018.04.06-macos/uaf.c
// https://opensource.apple.com/source/xnu/xnu-7195.101.1/tests/recvmsg_x_test.c.auto.html
// SYS_SENDMSG_X def:
// https://go.googlesource.com/sys/+/refs/heads/release-branch.go1.11/unix/zsysnum_darwin_386.go
const SYS_SENDMSG_X: usize = 481;

// Send multiple udp packets with a single system call.
//
// Each packet will contain utp_header with an increasing seq_nr.
// send_buffer will be split into packet_size chunks

#[cfg(target_os = "macos")]
pub fn sendmmsg(
    mut block: Block,
    mut utp_header: Header,
    packet_size: usize,
    socket: UdpSocket,
    addr: SocketAddr,
) -> std::io::Result<usize> {
    // Mostly for the lulz

    // Every packet must have a Header.
    // The first packet must contain the 13 byte prefix of Block
    // Every packet must be at most packet_size
    //
    // First block amount = packet_size - header - block_prefix = packet_size - 33
    // Every other block amount = packet_size - 20
    // BlockSize = packet_size - 33 + (N-1)*(packet_size - 20)
    // Solve for N and you get:
    let num_packets = (block.block.len() + 13).div_ceil(packet_size - 20);
    // TODO: these alloc.  Put all this in a struct so that we can reuse them
    let mut msghdr_xs = vec![MsghdrX::default(); num_packets];
    let mut aux_data = vec![
        (
            [libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            }; 2],
            // Wasting N_packets*13 bytes of space...
            ([0u8; Header::SIZE + 13], Header::SIZE),
        );
        num_packets
    ];
    let (mut sockaddr_in, sockaddr_len) = socketaddr_to_raw(addr);

    // Set first packet
    let mut first_packet_header = [0u8; 33];
    utp_header
        .write_to(&mut first_packet_header.as_mut_slice())
        .unwrap();
    first_packet_header[20..].copy_from_slice(&block.prefix());
    aux_data[0].1 = (first_packet_header, first_packet_header.len());
    let mut current_index = 0;
    for (c_struct, (iovecs, (header_buffer, header_len))) in
        msghdr_xs.iter_mut().zip(aux_data.iter_mut())
    {
        let block_len_sent =
            std::cmp::min(packet_size - *header_len, block.block.len() - current_index);
        utp_header
            .write_to(&mut (*header_buffer).as_mut_slice())
            .unwrap();
        utp_header.seq_nr += 1;
        *iovecs = [
            libc::iovec {
                iov_base: header_buffer.as_mut_ptr() as _,
                iov_len: *header_len,
            },
            libc::iovec {
                iov_base: unsafe { block.block.as_mut_ptr().add(current_index) as _ },
                iov_len: block_len_sent,
            },
        ];
        c_struct.msg_iov = iovecs.as_mut_ptr() as _;
        c_struct.msg_iovlen = iovecs.len() as _;
        c_struct.msg_name = sockaddr_in.as_mut_ptr() as _;
        c_struct.msg_namelen = sockaddr_len as _;
        current_index += block_len_sent;
    }
    let num_packets_sent = unsafe {
        libc::syscall(
            SYS_SENDMSG_X as _,
            socket.as_raw_fd() as usize,
            msghdr_xs.as_mut_ptr() as usize,
            msghdr_xs.len(),
            0,
        )
    };
    if num_packets_sent < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(num_packets_sent as _)
}

#[derive(Default)]
pub struct UtpBlockSender {
    msghdr_xs: Vec<MsghdrX>,
    aux_data: Vec<AuxData>,
    addrs: Vec<([u8; std::mem::size_of::<libc::sockaddr_in6>()], usize)>,
    high_watermark: usize,
}

#[derive(Clone)]
struct HeaderBuffer {
    buffer: [u8; 33],
    length: usize,
}

impl Default for HeaderBuffer {
    fn default() -> Self {
        Self {
            buffer: [0; 33],
            length: Header::SIZE,
        }
    }
}

#[derive(Clone)]
struct AuxData {
    iovecs: [libc::iovec; 2],
    header_buffer: HeaderBuffer,
}

impl Default for AuxData {
    fn default() -> Self {
        Self {
            iovecs: [libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            }; 2],
            header_buffer: Default::default(),
        }
    }
}

impl UtpBlockSender {
    const DEFAULT_PACKET_SIZE: usize = 1500;

    pub fn new() -> Self {
        let num_packets = (BLOCK_LENGTH + 13).div_ceil(Self::DEFAULT_PACKET_SIZE - 20);
        let msghdr_xs = vec![MsghdrX::default(); num_packets];
        let aux_data = vec![AuxData::default(); num_packets];
        Self {
            msghdr_xs,
            aux_data,
            high_watermark: num_packets,
            addrs: Vec::new(),
        }
    }

    pub fn send(
        &mut self,
        utp_header: Header,
        socket: &UdpSocket,
        packet_size: usize,
        data: &mut [u8],
        addr: SocketAddr,
    ) -> std::io::Result<usize> {
        self.send_internal(utp_header, socket, packet_size, data, addr, None)
    }

    pub fn send_block(
        &mut self,
        utp_header: Header,
        socket: &UdpSocket,
        packet_size: usize,
        mut block: Block,
        addr: SocketAddr,
    ) -> std::io::Result<usize> {
        let block_prefix = block.prefix();
        self.send_internal(
            utp_header,
            socket,
            packet_size,
            &mut block.block,
            addr,
            Some(&block_prefix),
        )
    }

    fn send_internal(
        &mut self,
        mut utp_header: Header,
        socket: &UdpSocket,
        packet_size: usize,
        data: &mut [u8],
        addr: SocketAddr,
        prefix: Option<&[u8]>,
    ) -> std::io::Result<usize> {
        let prefix_len = prefix.map_or(0, |x| x.len());
        let num_packets = (data.len() + prefix_len).div_ceil(packet_size - 20);
        self.resize(num_packets);
        let (mut sockaddr_in, sockaddr_len) = socketaddr_to_raw(addr);
        if let Some(prefix) = prefix {
            self.aux_data[0].header_buffer.buffer[20..].copy_from_slice(prefix);
            self.aux_data[0].header_buffer.length = Header::SIZE + prefix_len;
        }
        let mut current_index = 0;
        for (
            c_struct,
            AuxData {
                iovecs,
                header_buffer:
                    HeaderBuffer {
                        buffer: header_buffer,
                        length: header_len,
                    },
            },
        ) in self.msghdr_xs.iter_mut().zip(self.aux_data.iter_mut())
        {
            utp_header
                .write_to(&mut (*header_buffer).as_mut_slice())
                .unwrap();
            utp_header.seq_nr += 1;
            // Either send the max size we can fit into packet_size or the remaining data.
            let block_len_sent =
                std::cmp::min(packet_size - *header_len, data.len() - current_index);
            *iovecs = [
                libc::iovec {
                    iov_base: header_buffer.as_mut_ptr() as _,
                    iov_len: *header_len,
                },
                libc::iovec {
                    iov_base: unsafe { data.as_mut_ptr().add(current_index) as _ },
                    iov_len: block_len_sent,
                },
            ];
            c_struct.msg_iov = iovecs.as_mut_ptr() as _;
            c_struct.msg_iovlen = iovecs.len() as _;
            c_struct.msg_name = sockaddr_in.as_mut_ptr() as _;
            c_struct.msg_namelen = sockaddr_len as _;
            current_index += block_len_sent;
        }
        let num_packets_sent = unsafe {
            libc::syscall(
                SYS_SENDMSG_X as _,
                socket.as_raw_fd() as usize,
                self.msghdr_xs.as_mut_ptr() as usize,
                self.msghdr_xs.len(),
                0,
            )
        };
        if num_packets_sent < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if num_packets_sent as usize != num_packets {
            // TODO: if this happens, I should retry the remaining packets either until I have sent all of the packets
            // or I get a negative return
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        Ok(num_packets_sent as _)
    }

    fn resize(&mut self, new_len: usize) {
        if new_len <= self.high_watermark {
            unsafe {
                // This is safe because these values have been previously initiallized,
                // and we will be overwriting them immediately.
                self.msghdr_xs.set_len(new_len);
                self.aux_data.set_len(new_len);
            }
        } else {
            self.msghdr_xs.resize(new_len, MsghdrX::default());
            self.aux_data.resize(new_len, AuxData::default());
            self.high_watermark = new_len
        }
    }

    // Send the same data to multiple destinations
    pub fn send_all(
        &mut self,
        connections: impl Iterator<Item = (SocketAddr, usize, Header)>,
        data: &mut [u8],
        socket: &UdpSocket,
    ) -> std::io::Result<()> {
        // assume it fits in packet for now
        let mut num_packets = 0;
        for (addr, _packet_size, header) in connections {
            self.aux_data.push(Default::default());
            let AuxData {
                iovecs,
                header_buffer,
            } = self.aux_data.last_mut().unwrap();
            header
                .write_to(&mut header_buffer.buffer.as_mut_slice())
                .expect("this write can't fail");
            *iovecs = [
                libc::iovec {
                    iov_base: header_buffer.buffer.as_mut_ptr() as _,
                    iov_len: Header::SIZE,
                },
                libc::iovec {
                    iov_base: data.as_mut_ptr() as _,
                    iov_len: data.len(),
                },
            ];
            self.addrs.push(Default::default());
            let (addr_buffer, addr_len) = self.addrs.last_mut().unwrap();
            let (sockaddr_in, sockaddr_len) = socketaddr_to_raw(addr);
            *addr_buffer = sockaddr_in;
            *addr_len = sockaddr_len;

            self.msghdr_xs.push(MsghdrX {
                msg_iov: iovecs.as_mut_ptr() as _,
                msg_iovlen: iovecs.len() as _,
                msg_name: addr_buffer.as_mut_ptr() as _,
                msg_namelen: *addr_len as _,
                ..Default::default()
            });
            num_packets += 1;
        }
        let num_packets_sent = unsafe {
            libc::syscall(
                SYS_SENDMSG_X as _,
                socket.as_raw_fd() as usize,
                self.msghdr_xs.as_mut_ptr() as usize,
                self.msghdr_xs.len(),
                0,
            )
        };
        if num_packets_sent < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if num_packets_sent as usize != num_packets {
            // TODO: if this happens, I should retry the remaining packets either until I have sent all of the packets
            // or I get a negative return
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }

        Ok(())
    }
}
