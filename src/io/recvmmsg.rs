use libc::iovec;
use std::net::SocketAddr;
use std::net::{IpAddr, Ipv4Addr};
use std::os::fd::AsRawFd;

use mio::net::UdpSocket;

use super::ReadBuffer;

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
    let mut syscall_data = msghdr_x_helper::new(recv_buffer);
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
    let result_addr = syscall_data.finalize(recv_buffer, num_packets_received as _);
    Ok((num_packets_received as _, result_addr))
}

/*
This is the actual C struct used by the SYS_RECVMSG_X syscall.

struct msghdr_x {
    void            *msg_name;      /* optional address */
    socklen_t       msg_namelen;    /* size of address */
    struct iovec    *msg_iov;       /* scatter/gather array */
    int             msg_iovlen;     /* # elements in msg_iov */
    void            *msg_control;   /* ancillary data, see below */
    socklen_t       msg_controllen; /* ancillary data buffer len */
    int             msg_flags;      /* flags on received message */
    size_t          msg_datalen;    /* byte length of buffer in msg_iov */
};
*/

// This is the raw data that is passed to the recvmsg_x syscall.
#[repr(C)]
#[derive(Copy, Clone)]
struct msghdr_x {
    msg_name: *mut libc::c_void,
    msg_namelen: libc::socklen_t,
    msg_iov: *mut libc::iovec,
    msg_iovlen: i32,
    msg_control: *mut libc::c_void,
    msg_controllen: libc::socklen_t,
    msg_flags: i32,
    msg_datalen: libc::size_t,
}

impl Default for msghdr_x {
    fn default() -> Self {
        Self {
            msg_name: std::ptr::null_mut(),
            msg_namelen: Default::default(),
            msg_iov: std::ptr::null_mut(),
            msg_iovlen: Default::default(),
            msg_control: std::ptr::null_mut(),
            msg_controllen: Default::default(),
            msg_flags: Default::default(),
            msg_datalen: Default::default(),
        }
    }
}

#[allow(non_camel_case_types)]
struct msghdr_x_helper<const N: usize> {
    inner: [msghdr_x; N],
    // These are never read, but need to live as long as inner because inner contains
    // mutable pointers to the data in both.
    #[allow(dead_code)]
    iovecs: [libc::iovec; N],
    #[allow(dead_code)]
    src_addrs: [[u8; std::mem::size_of::<libc::sockaddr_in6>()]; N],
}

impl<const N: usize> msghdr_x_helper<N> {
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
    ) -> [SocketAddr; N] {
        const IPV4: usize = std::mem::size_of::<libc::sockaddr_in>();
        const IPV6: usize = std::mem::size_of::<libc::sockaddr_in6>();
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
            let src_addr_raw = c_struct.msg_name as *const u8;
            let src_addr_raw =
                unsafe { std::slice::from_raw_parts(src_addr_raw, c_struct.msg_namelen as usize) };
            match src_addr_raw.len() {
                IPV4 => {
                    let addr_raw: [u8; IPV4] = src_addr_raw.try_into().unwrap();
                    let addr_raw: libc::sockaddr_in = unsafe { std::mem::transmute(addr_raw) };
                    let ip_raw = addr_raw.sin_addr.s_addr.swap_bytes();
                    addr.set_ip(IpAddr::V4(ip_raw.into()));
                    addr.set_port(addr_raw.sin_port.swap_bytes());
                }
                IPV6 => {
                    let addr_raw: [u8; IPV6] = src_addr_raw.try_into().unwrap();
                    let addr_raw: libc::sockaddr_in6 = unsafe { std::mem::transmute(addr_raw) };
                    let ip_raw = addr_raw.sin6_addr.s6_addr;
                    addr.set_ip(IpAddr::V6(ip_raw.into()));
                    addr.set_port(addr_raw.sin6_port.swap_bytes());
                }
                _ => panic!("Unknown addr size: {}", c_struct.msg_namelen),
            }
        }
        result_addr
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        common::{new_udp_socket, new_udp_socket_ipv6},
        io::ReadBuffer,
    };

    use super::recvmmsg;

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
}
