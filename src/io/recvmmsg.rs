use libc::iovec;
use std::net::SocketAddr;
use std::net::{IpAddr, Ipv4Addr};
use std::os::fd::AsRawFd;

use mio::net::UdpSocket;

use crate::io::raw_to_socketaddr;

use super::{msghdr_x, ReadBuffer};

// Receive multiple udp packets with a single system call.
#[cfg(target_os = "macos")]
pub fn recvmmsg<const N: usize>(
    recv_buffer: &mut [ReadBuffer; N],
    socket: UdpSocket,
) -> std::io::Result<(usize, [SocketAddr; N])> {
    // examples which helped implement this:
    // https://gist.github.com/Maximus-/487c70508e161680d550ccb343781859
    // https://github.com/grimm-co/NotQuite0DayFriday/blob/trunk/2018.04.06-macos/uaf.c
    // https://opensource.apple.com/source/xnu/xnu-7195.101.1/tests/recvmsg_x_test.c.auto.html
    // SYS_RECVMSG_X def:
    // https://go.googlesource.com/sys/+/refs/heads/release-branch.go1.11/unix/zsysnum_darwin_386.go
    const SYS_RECVMSG_X: usize = 480;
    let mut syscall_data = msghdr_x_recv_helper::new(recv_buffer);
    let num_packets_received = unsafe {
        libc::syscall(
            SYS_RECVMSG_X as _,
            socket.as_raw_fd() as usize,
            syscall_data.get_msghdr_x_address(),
            N,
            0,
        )
    };
    if num_packets_received < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let result_addr = syscall_data.finalize(recv_buffer, num_packets_received as _)?;
    Ok((num_packets_received as _, result_addr))
}

#[allow(non_camel_case_types)]
struct msghdr_x_recv_helper<const N: usize> {
    inner: [msghdr_x; N],
    // These are never read, but need to live as long as inner because inner contains
    // mutable pointers to the data in both.
    #[allow(dead_code)]
    iovecs: [libc::iovec; N],
    #[allow(dead_code)]
    src_addrs: [[u8; std::mem::size_of::<libc::sockaddr_in6>()]; N],
}

impl<const N: usize> msghdr_x_recv_helper<N> {
    pub fn new(recv_buffer: &mut [ReadBuffer; N]) -> Self {
        // Need to construct them inside of self first, otherwise the addresses will change
        // when they are copied into self.  Probably safer to pin the object, but I
        // want to avoid the allocation required to pin it to the heap.
        let mut result = Self {
            inner: [msghdr_x::default(); N],
            iovecs: [libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            }; N],
            src_addrs: [[0u8; std::mem::size_of::<libc::sockaddr_in6>()]; N],
        };
        for (((buffer, c_struct), iovec), src_addr) in recv_buffer
            .iter_mut()
            .zip(result.inner.iter_mut())
            .zip(result.iovecs.iter_mut())
            .zip(result.src_addrs.iter_mut())
        {
            // msg_iov is filled with the packet data, so we put pointers
            // to recv_buffer in it so recv_buffer is directly filled.
            *iovec = libc::iovec {
                iov_base: buffer.get_unused_mut().as_mut_ptr() as _,
                iov_len: buffer.unused(),
            };
            c_struct.msg_iov = iovec as *mut iovec;
            c_struct.msg_iovlen = 1;
            // msg_name is populated by the source address of the packet.
            c_struct.msg_name = src_addr.as_mut_ptr() as _;
            c_struct.msg_namelen = src_addr.len() as u32;
        }
        result
    }

    pub fn get_msghdr_x_address(&mut self) -> usize {
        self.inner.as_mut_ptr() as _
    }

    // Update the size of recv_buffers and returns the source address of the packets.
    //
    // Only num_packets_received of the returned array will be filled.
    pub fn finalize(
        &self,
        recv_buffer: &mut [ReadBuffer; N],
        num_packets_received: usize,
    ) -> std::io::Result<[SocketAddr; N]> {
        let mut result_addr = [SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0); N];
        for ((result_buffer, c_struct), addr) in recv_buffer
            .iter_mut()
            .zip(self.inner.iter())
            .zip(result_addr.iter_mut())
            .take(num_packets_received as _)
        {
            // datalen has been filled with the actual size of the packet.
            result_buffer.added_unused(c_struct.msg_datalen);
            // msg_name has been filled with the source address, but we have to do some work to
            // convert them to useful types.
            *addr = raw_to_socketaddr(c_struct.msg_name, c_struct.msg_namelen)?;
        }
        Ok(result_addr)
    }
}

#[cfg(test)]
mod tests {

    use std::net::SocketAddr;
    use write_to::ReadFrom;

    use crate::{
        client::utp::Header,
        common::{new_udp_socket, new_udp_socket_ipv6},
        io::{
            sendmmsg::{sendmmsg, UtpBlockSender},
            sockaddr_in_to_socketaddr, socketaddrv4_to_sockaddr_in, ReadBuffer,
        },
        messages::{protocol_message::HasId, read_byte, Block},
    };

    use super::recvmmsg;
    #[test]
    fn test_sockaddr_convert() {
        let socket = new_udp_socket();
        let addr = socket.local_addr().unwrap();
        let SocketAddr::V4(ipv4) = addr else {
            panic!("unexpected ipv6")
        };
        let sockaddr_in = socketaddrv4_to_sockaddr_in(&ipv4);
        assert_eq!(ipv4, sockaddr_in_to_socketaddr(sockaddr_in));
    }

    #[test]
    fn test_size_0() {
        let recv = new_udp_socket();
        let mut recv_buffer = [ReadBuffer::new(100)];
        let result = recvmmsg(&mut recv_buffer, recv).unwrap_err();
        assert_eq!(result.kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn test_size_1() {
        let send = new_udp_socket();
        let recv = new_udp_socket();
        let data_to_send = "hi hi how are you";
        let _ = send.send_to(data_to_send.as_bytes(), recv.local_addr().unwrap());
        let mut recv_buffer = [ReadBuffer::new(100)];
        std::thread::sleep(std::time::Duration::from_millis(100));
        let (n_recv, addrs) = recvmmsg(&mut recv_buffer, recv).expect("no err");
        assert_eq!(n_recv, 1);
        let actual = String::from_utf8(recv_buffer[0].get_unread().to_vec()).unwrap();
        assert_eq!(data_to_send, actual);
        assert_eq!(send.local_addr().unwrap(), addrs[0]);
    }

    #[test]
    fn test_size_1_ipv6() {
        let send = new_udp_socket_ipv6();
        let recv = new_udp_socket_ipv6();
        let data_to_send = "hi hi how are you";
        let _ = send.send_to(data_to_send.as_bytes(), recv.local_addr().unwrap());
        let mut recv_buffer = [ReadBuffer::new(100)];
        std::thread::sleep(std::time::Duration::from_millis(100));
        let (n_recv, addrs) = recvmmsg(&mut recv_buffer, recv).expect("no err");
        assert_eq!(n_recv, 1);
        let actual = String::from_utf8(recv_buffer[0].get_unread().to_vec()).unwrap();
        assert_eq!(data_to_send, actual);
        assert_eq!(send.local_addr().unwrap(), addrs[0]);
    }

    #[test]
    fn test_size_5() {
        let send = new_udp_socket();
        let recv = new_udp_socket();
        let data_to_send = "hi hi how are you";
        let _ = send.send_to(data_to_send.as_bytes(), recv.local_addr().unwrap());
        let _ = send.send_to(data_to_send.as_bytes(), recv.local_addr().unwrap());
        let _ = send.send_to(data_to_send.as_bytes(), recv.local_addr().unwrap());
        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut recv_buffer = [
            ReadBuffer::new(100),
            ReadBuffer::new(100),
            ReadBuffer::new(100),
            ReadBuffer::new(100),
        ];
        let (n_recv, _) = recvmmsg(&mut recv_buffer, recv).expect("no err");
        assert_eq!(n_recv, 3);
        let get_recv = |i: usize| {
            let actual = String::from_utf8(recv_buffer[i].get_unread().to_vec()).unwrap();
            actual
        };
        assert_eq!(data_to_send, get_recv(0));
        assert_eq!(data_to_send, get_recv(1));
        assert_eq!(data_to_send, get_recv(2));
    }

    #[test]
    fn test_send_block() {
        let random_bytes: Vec<u8> = (0..1024).map(|_| rand::random::<u8>()).collect();
        let block = Block {
            index: 731,
            begin: 217,
            block: random_bytes,
        };
        let send = new_udp_socket();
        let recv = new_udp_socket();
        // let sent = sendmmsg(
        //     block.clone(),
        //     Header::new(
        //         crate::client::utp::Type::StData,
        //         1,
        //         176,
        //         12000,
        //         1000,
        //         0,
        //         1500,
        //     ),
        //     117,
        //     send,
        //     recv.local_addr().unwrap(),
        // )
        // .unwrap();
        let mut block_sender = UtpBlockSender::new();
        let sent = block_sender
            .send(
                Header::new(
                    crate::client::utp::Type::StData,
                    1,
                    176,
                    12000,
                    1000,
                    0,
                    1500,
                ),
                &send,
                117,
                block.clone(),
                recv.local_addr().unwrap(),
            )
            .unwrap();
        let mut recv_buffer = [
            ReadBuffer::new(180),
            ReadBuffer::new(180),
            ReadBuffer::new(180),
            ReadBuffer::new(180),
            ReadBuffer::new(180),
            ReadBuffer::new(180),
            ReadBuffer::new(180),
            ReadBuffer::new(180),
            ReadBuffer::new(180),
            ReadBuffer::new(180),
            ReadBuffer::new(180),
            ReadBuffer::new(180),
        ];
        let (n_recv, _) = recvmmsg(&mut recv_buffer, recv).expect("no err");
        assert_eq!(n_recv, sent);
        // Read Length, Byte, index, begin, then block
        let mut recv_block = Vec::new();
        let remaining = recv_buffer[0].unread();
        let (_, remaining) = Header::read_from(&mut recv_buffer[0], remaining).unwrap();
        let (_, remaining) = u32::read_from(&mut recv_buffer[0], remaining).unwrap();
        let id = read_byte(&mut recv_buffer[0]).unwrap();
        assert_eq!(id, Block::ID);
        let (index, remaining) = u32::read_from(&mut recv_buffer[0], remaining).unwrap();
        assert_eq!(index, 731);
        let (begin, _) = u32::read_from(&mut recv_buffer[0], remaining).unwrap();
        assert_eq!(begin, 217);
        // let (block_piece, _) = Vec::<u8>::read_from(&mut recv_buffer[0], remaining);
        recv_block.extend(recv_buffer[0].get_unread());
        for (i, packet) in recv_buffer.iter_mut().skip(1).enumerate() {
            if packet.unread() == 0 {
                println!("{i} packet has no data");
                continue;
            }
            println!("{i} packet has data: {}", packet.unread());
            let (_, _) = Header::read_from(packet, packet.unread()).unwrap();
            recv_block.extend(packet.get_unread());
        }
        assert_eq!(block.block.len(), recv_block.len());
        assert_eq!(block.block, recv_block);
    }

    #[test]
    fn test_send_1_ipv4() {
        let random_bytes: Vec<u8> = (0..1024).map(|_| rand::random::<u8>()).collect();
        let block = Block {
            index: 731,
            begin: 217,
            block: random_bytes,
        };
        let send = new_udp_socket();
        let recv = new_udp_socket();
        let sent = sendmmsg(
            block,
            Header::new(
                crate::client::utp::Type::StData,
                1,
                176,
                12000,
                1000,
                0,
                1500,
            ),
            1500,
            send,
            recv.local_addr().unwrap(),
        )
        .unwrap();
        let mut recv_buffer = [ReadBuffer::new(1600), ReadBuffer::new(180)];
        let (n_recv, _) = recvmmsg(&mut recv_buffer, recv).expect("no err");
        assert_eq!(n_recv, sent);
    }

    #[test]
    fn test_send_1_ipv6() {
        let random_bytes: Vec<u8> = (0..1024).map(|_| rand::random::<u8>()).collect();
        let block = Block {
            index: 731,
            begin: 217,
            block: random_bytes,
        };
        let send = new_udp_socket_ipv6();
        let recv = new_udp_socket_ipv6();
        let sent = sendmmsg(
            block,
            Header::new(
                crate::client::utp::Type::StData,
                1,
                176,
                12000,
                1000,
                0,
                1500,
            ),
            1500,
            send,
            recv.local_addr().unwrap(),
        )
        .unwrap();
        let mut recv_buffer = [ReadBuffer::new(1600), ReadBuffer::new(180)];
        let (n_recv, _) = recvmmsg(&mut recv_buffer, recv).expect("no err");
        assert_eq!(n_recv, sent);
    }
}
