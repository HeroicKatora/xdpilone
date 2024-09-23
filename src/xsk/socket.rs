use alloc::sync::Arc;

use crate::xsk::{IfInfo, Socket, SocketFd, Umem};
use crate::{Errno, LastErrno};

impl Socket {
    const SO_NETNS_COOKIE: libc::c_int = 71;
    const INIT_NS: u64 = 1;

    /// Create a new socket for a given interface.
    pub fn new(interface: &IfInfo) -> Result<Self, Errno> {
        let fd = Arc::new(SocketFd::new()?);
        Self::with_xdp_socket(interface, fd)
    }

    /// Create a socket using the FD of the `umem`.
    pub fn with_shared(interface: &IfInfo, umem: &Umem) -> Result<Self, Errno> {
        Self::with_xdp_socket(interface, umem.fd.clone())
    }

    fn with_xdp_socket(interface: &IfInfo, fd: Arc<SocketFd>) -> Result<Self, Errno> {
        let mut info = Arc::new(*interface);

        let mut netnscookie: u64 = 0;
        let mut optlen: libc::socklen_t = core::mem::size_of_val(&netnscookie) as libc::socklen_t;
        let err = unsafe {
            libc::getsockopt(
                fd.0,
                libc::SOL_SOCKET,
                Self::SO_NETNS_COOKIE,
                (&mut netnscookie) as *mut _ as *mut libc::c_void,
                &mut optlen,
            )
        };

        match err {
            0 => {}
            libc::ENOPROTOOPT => netnscookie = Self::INIT_NS,
            _ => return Err(LastErrno)?,
        }

        // Won't reallocate in practice.
        Arc::make_mut(&mut info).ctx.netnscookie = netnscookie;

        Ok(Socket { fd, info })
    }
}

impl SocketFd {
    pub(crate) fn new() -> Result<Self, Errno> {
        let fd = unsafe { libc::socket(libc::AF_XDP, libc::SOCK_RAW, 0) };
        if fd < 0 {
            return Err(LastErrno)?;
        }
        Ok(SocketFd(fd))
    }
}
