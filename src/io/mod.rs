pub mod read_buffer;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

pub use read_buffer::*;
pub mod recvmmsg;
pub mod sendmmsg;

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
#[repr(C)]
#[derive(Copy, Clone)]
struct MsghdrX {
    msg_name: *mut libc::c_void,
    msg_namelen: libc::socklen_t,
    msg_iov: *mut libc::iovec,
    msg_iovlen: i32,
    msg_control: *mut libc::c_void,
    msg_controllen: libc::socklen_t,
    msg_flags: i32,
    msg_datalen: libc::size_t,
}

impl Default for MsghdrX {
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

pub fn sockaddr_in_to_socketaddr(source: libc::sockaddr_in) -> SocketAddrV4 {
    let ip_raw = source.sin_addr.s_addr.swap_bytes();
    SocketAddrV4::new(ip_raw.into(), source.sin_port.swap_bytes())
}

pub fn socketaddrv4_to_sockaddr_in(ipv4: &SocketAddrV4) -> libc::sockaddr_in {
    let port = ipv4.port();
    let ip = ipv4.ip();
    println!("ipv4: {ip:?}");
    let ip_raw: u32 = (*ip).into();
    libc::sockaddr_in {
        sin_len: 0, // ?
        sin_family: libc::AF_INET as _,
        sin_port: port.swap_bytes(), // ?
        sin_addr: libc::in_addr {
            s_addr: ip_raw.swap_bytes(),
        },
        sin_zero: [0; 8],
    }
}

pub fn socketaddrv6_to_sockaddr_in6(ipv6: &SocketAddrV6) -> libc::sockaddr_in6 {
    let port = ipv6.port();
    let ip = ipv6.ip();
    println!("ipv6: {ip:?}");
    let ip_raw: u128 = (*ip).into();
    libc::sockaddr_in6 {
        sin6_len: 0,
        sin6_family: libc::AF_INET6 as _,
        sin6_port: port.swap_bytes(),
        sin6_flowinfo: 0,
        sin6_addr: libc::in6_addr {
            s6_addr: ip_raw.to_be_bytes(),
        },
        sin6_scope_id: 0,
    }
}

pub fn raw_to_socketaddr(src: *const libc::c_void, src_len: u32) -> std::io::Result<SocketAddr> {
    const IPV4: usize = std::mem::size_of::<libc::sockaddr_in>();
    const IPV6: usize = std::mem::size_of::<libc::sockaddr_in6>();
    let mut result = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
    let src_addr_raw = src as *const u8;
    let src_addr_raw = unsafe { std::slice::from_raw_parts(src_addr_raw, src_len as usize) };
    match src_addr_raw.len() {
        IPV4 => {
            let addr_raw: [u8; IPV4] = src_addr_raw.try_into().unwrap();
            let addr_raw: libc::sockaddr_in = unsafe { std::mem::transmute(addr_raw) };
            let ip_raw = addr_raw.sin_addr.s_addr.swap_bytes();
            result.set_ip(IpAddr::V4(ip_raw.into()));
            result.set_port(addr_raw.sin_port.swap_bytes());
        }
        IPV6 => {
            let addr_raw: [u8; IPV6] = src_addr_raw.try_into().unwrap();
            let addr_raw: libc::sockaddr_in6 = unsafe { std::mem::transmute(addr_raw) };
            let ip_raw = addr_raw.sin6_addr.s6_addr;
            result.set_ip(IpAddr::V6(ip_raw.into()));
            result.set_port(addr_raw.sin6_port.swap_bytes());
        }
        _ => return Err(std::io::ErrorKind::InvalidData.into()),
    }
    Ok(result)
}

pub fn socketaddr_to_raw(
    src: SocketAddr,
) -> ([u8; std::mem::size_of::<libc::sockaddr_in6>()], usize) {
    let mut sockaddr_in = [0u8; std::mem::size_of::<libc::sockaddr_in6>()];
    let sockaddr_len = match src {
        SocketAddr::V4(ipv4) => {
            let sockaddr = socketaddrv4_to_sockaddr_in(&ipv4);
            unsafe {
                sockaddr_in[0..std::mem::size_of::<libc::sockaddr_in>()].copy_from_slice(
                    std::slice::from_raw_parts(
                        ((&sockaddr) as *const libc::sockaddr_in) as *const u8,
                        std::mem::size_of::<libc::sockaddr_in>(),
                    ),
                )
            };
            std::mem::size_of::<libc::sockaddr_in>()
        }
        SocketAddr::V6(ipv6) => {
            let sockaddr = socketaddrv6_to_sockaddr_in6(&ipv6);
            unsafe {
                sockaddr_in[0..std::mem::size_of::<libc::sockaddr_in6>()].copy_from_slice(
                    std::slice::from_raw_parts(
                        ((&sockaddr) as *const libc::sockaddr_in6) as *const u8,
                        std::mem::size_of::<libc::sockaddr_in6>(),
                    ),
                )
            };
            std::mem::size_of::<libc::sockaddr_in6>()
        }
    };
    (sockaddr_in, sockaddr_len)
}
