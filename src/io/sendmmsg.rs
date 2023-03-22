use std::net::SocketAddr;
use std::os::fd::AsRawFd;

use mio::net::UdpSocket;

use super::msghdr_x;
use crate::client::utp::Header;
use crate::io::socketaddr_to_raw;
use crate::messages::Block;

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
    use write_to::WriteTo;

    // examples which helped implement this:
    // https://gist.github.com/Maximus-/487c70508e161680d550ccb343781859
    // https://github.com/grimm-co/NotQuite0DayFriday/blob/trunk/2018.04.06-macos/uaf.c
    // https://opensource.apple.com/source/xnu/xnu-7195.101.1/tests/recvmsg_x_test.c.auto.html
    // SYS_SENDMSG_X def:
    // https://go.googlesource.com/sys/+/refs/heads/release-branch.go1.11/unix/zsysnum_darwin_386.go
    const SYS_SENDMSG_X: usize = 481;

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
    let mut msghdr_xs = vec![msghdr_x::default(); num_packets];
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
            msghdr_xs.len() as usize,
            0,
        )
    };
    if num_packets_sent < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(num_packets_sent as _)
}
