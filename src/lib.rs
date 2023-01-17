//! Rust idiomatic bindings for the AF_XDP socket interface.
//!
//! This library helps with creating suitable socket(s) from a memory allocation of chunks, sockets
//! for access to all four rings, binding to a specific `(ifname, queue_id)`, and for creating the
//! memory mapping to interact with all these queues directly.
//!
//! It does _not_ interact with the packet filter / forwarding directly, nor any other aspect of
//! `bpf`. You can send packets but you can not receive them. Please use another library for `bpf`
//! and `netlink` interaction to configure the network device to route received frames to the RX
//! ring.
//!
//! The entrypoint to the library is an instance of [`xsk::Umem`].
#![no_std]
#![deny(missing_docs)]
extern crate alloc;

/// User-space side of one or multiple XDP sockets.
pub mod xsk;
/// Bindings for XDP kernel-interface, including structs.
pub mod xdp;

pub(crate) struct LastErrno;

/// An error that has been read from `errno`.
pub struct Errno(libc::c_int);

impl From<LastErrno> for Errno {
    fn from(LastErrno: LastErrno) -> Self {
        Errno::new()
    }
}

impl Errno {
    /// Create an error from the latest `errno`.
    pub fn new() -> Self {
        Errno(unsafe { *libc::__errno_location() })
    }

    /// Get the actual `errno` value.
    pub fn get_raw(&self) -> libc::c_int {
        self.0
    }
}

impl core::fmt::Display for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let st = unsafe { libc::strerror(self.0) };
        let cstr = unsafe { core::ffi::CStr::from_ptr(st) };
        write!(f, "{}", cstr.to_string_lossy())
    }
}

impl core::fmt::Debug for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Errno({}: {})", self.0, self)
    }
}
