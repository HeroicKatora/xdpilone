use core::ffi::CStr;

use super::{IfCtx, IfInfo, SocketFd, SocketMmapOffsets};
use crate::xdp::{XdpMmapOffsets, XdpMmapOffsetsV1, XdpStatistics};
use crate::{Errno, LastErrno};

impl IfInfo {
    /// Create an info referring to no device.
    ///
    /// This allows allocating an info to overwrite with more specific information.
    pub fn invalid() -> Self {
        IfInfo {
            ctx: IfCtx {
                ifindex: 0,
                queue_id: 0,
                netnscookie: 0,
            },
            ifname: [b'\0' as libc::c_char; libc::IFNAMSIZ],
        }
    }

    /// Set the information from an interface, by name.
    ///
    /// Common interface names may be `enp8s0`, `lo`, `wg0`, etc. The interface name-to-index pair
    /// will be very similar to what would be returned by `ip link show`.
    pub fn from_name(&mut self, st: &CStr) -> Result<(), Errno> {
        let bytes = st.to_bytes_with_nul();

        if bytes.len() > self.ifname.len() {
            return Err(Errno(libc::EINVAL));
        }

        assert!(bytes.len() <= self.ifname.len());
        let bytes = unsafe { &*(bytes as *const _ as *const [libc::c_char]) };
        let index = unsafe { libc::if_nametoindex(st.as_ptr()) };

        if index == 0 {
            return Err(LastErrno)?;
        }

        self.ctx.ifindex = index;
        self.ctx.queue_id = 0;
        self.ctx.netnscookie = 0;
        self.ifname[..bytes.len()].copy_from_slice(bytes);

        Ok(())
    }

    /// Set the information from an interface, by its numeric identifier.
    ///
    /// See [`Self::from_name`].
    pub fn from_ifindex(&mut self, index: libc::c_uint) -> Result<(), Errno> {
        let err = unsafe { libc::if_indextoname(index, self.ifname.as_mut_ptr()) };

        if err.is_null() {
            return Err(LastErrno)?;
        }

        Ok(())
    }

    /// Configure the QueueID.
    ///
    /// This does _not_ guarantee that this queue is valid, or actually exists. You'll find out
    /// during the bind call. Most other ways of querying such information could suffer from TOCTOU
    /// issues in any case.
    pub fn set_queue(&mut self, queue_id: u32) {
        self.ctx.queue_id = queue_id;
    }

    /// Get the `ifindex`, numeric ID of the interface in the kernel, for the identified interface.
    pub fn ifindex(&self) -> u32 {
        self.ctx.ifindex
    }

    /// Get the queue ID previously set with `set_queue`.
    pub fn queue_id(&self) -> u32 {
        self.ctx.queue_id
    }
}

impl SocketMmapOffsets {
    const OPT_V1: libc::socklen_t = core::mem::size_of::<XdpMmapOffsetsV1>() as libc::socklen_t;
    const OPT_LATEST: libc::socklen_t = core::mem::size_of::<XdpMmapOffsets>() as libc::socklen_t;

    /// Query the socket mmap offsets of an XDP socket.
    pub fn new(sock: &SocketFd) -> Result<Self, Errno> {
        let mut this = SocketMmapOffsets {
            inner: Default::default(),
        };
        this.set_from_fd(sock)?;
        Ok(this)
    }

    /// Overwrite data with the socket mmap offsets of an XDP socket.
    ///
    /// This operation is atomic: On error, the previous values are retained. On success, the
    /// attributes have been updated.
    pub fn set_from_fd(&mut self, sock: &SocketFd) -> Result<(), Errno> {
        use crate::xdp::{XdpRingOffsets, XdpRingOffsetsV1};

        // The flags was implicit, based on the consumer.
        fn fixup_v1(v1: XdpRingOffsetsV1) -> XdpRingOffsets {
            XdpRingOffsets {
                producer: v1.producer,
                consumer: v1.consumer,
                desc: v1.desc,
                flags: v1.consumer + core::mem::size_of::<u32>() as u64,
            }
        }

        union Offsets {
            v1: XdpMmapOffsetsV1,
            latest: XdpMmapOffsets,
            init: (),
        }

        let mut off = Offsets { init: () };
        let mut optlen: libc::socklen_t = core::mem::size_of_val(&off) as libc::socklen_t;

        let err = unsafe {
            libc::getsockopt(
                sock.0,
                super::SOL_XDP,
                super::Umem::XDP_MMAP_OFFSETS,
                (&mut off) as *mut _ as *mut libc::c_void,
                &mut optlen,
            )
        };

        if err != 0 {
            return Err(LastErrno)?;
        }

        match optlen {
            Self::OPT_V1 => {
                let v1 = unsafe { off.v1 };

                self.inner = XdpMmapOffsets {
                    rx: fixup_v1(v1.rx),
                    tx: fixup_v1(v1.tx),
                    fr: fixup_v1(v1.fr),
                    cr: fixup_v1(v1.cr),
                };

                Ok(())
            }
            Self::OPT_LATEST => {
                self.inner = unsafe { off.latest };
                Ok(())
            }
            _ => Err(Errno(-libc::EINVAL)),
        }
    }
}

impl XdpStatistics {
    pub(crate) fn new(sock: &SocketFd) -> Result<Self, Errno> {
        let mut this = Self::default();
        this.set_from_fd(sock)?;
        Ok(this)
    }

    pub(crate) fn set_from_fd(&mut self, sock: &SocketFd) -> Result<(), Errno> {
        let mut optlen: libc::socklen_t = core::mem::size_of_val(self) as libc::socklen_t;
        let err = unsafe {
            libc::getsockopt(
                sock.0,
                super::SOL_XDP,
                super::Umem::XDP_STATISTICS,
                self as *mut _ as *mut libc::c_void,
                &mut optlen,
            )
        };

        if err != 0 {
            return Err(LastErrno)?;
        }

        Ok(())
    }
}
