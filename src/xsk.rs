//! Our own XSK (user-space XDP ring implementation).
//!
//! Consider: the reasoning behind these structs is their implementation in a _header_ of C code,
//! so that they can be optimized on all platforms. How much sense does it make to not write them
//! in Rust code, where rustc does _exactly_ this.
//!
//! The data structures here are not *safe* to construct. Some of them depend on the caller to
//! uphold guarantees such as keeping an mmap alive, or holding onto a socket for them. Take care.

/// Implementations for primitives `XskRing`, `XskRingProd`, `XskRingCons`.
mod ring;

/// Implementations for interface related operations.
mod iface;

use crate::xdp::{XdpMmapOffsets, XdpUmemReg};

use alloc::sync::Arc;
use core::ptr::NonNull;
use core::sync::atomic::AtomicU32;

struct SocketFd(libc::c_int);

/// Internal structure shared for all rings.
///
/// TODO: copied from <xdp.h>, does everything make sense in Rust?
#[repr(C)]
#[derive(Debug)]
struct XskRing {
    /// _owned_ version of the producer head, may lag.
    cached_producer: u32,
    /// _owned_ version of the consumer head, may lag.
    cached_consumer: u32,
    /// Bit mask to quickly validate/force entry IDs.
    mask: u32,
    /// Number of entries (= mask + 1).
    size: u32,
    /// The mmaped-producer base.
    ///
    /// Note: Using lifetime static here, but we point into an `mmap` area and it is important that
    /// we do not outlive the binding. The constructor promises this.
    producer: &'static AtomicU32,
    /// The mmaped-consumer base.
    consumer: &'static AtomicU32,
    /// The mmaped-consumer ring control base.
    ring: NonNull<core::ffi::c_void>,
    /// The mmaped-consumer flags base.
    flags: NonNull<u32>,
}

pub struct XskUmemConfig {
    /// Number of entries in the fill queue.
    pub fill_size: u32,
    /// Number of entries in the completion queue.
    pub complete_size: u32,
    /// Size of data frames in the queues.
    pub frame_size: u32,
    /// Reserved area at the start of the kernel area.
    pub headroom: u32,
    /// Flags to set with the creation calls.
    pub flags: u32,
}

pub struct XskSocketConfig {
    pub rx_size: u32,
    pub tx_size: u32,
    pub flags: u32,
    pub xdp_flags: u32,
}

/// The basic Umem descriptor.
///
/// Compared to `libxdp` there no link to the queues is stored. Such a struct would necessitate
/// thread-safe access to the ring's producer and consumer queues. Instead, a `XskDevice` is the
/// owner of a device queue's fill/completion ring, but _not_ receive and transmission rings. All
/// other sockets with the same interface/queue depend on it but have their own packet rings.
///
/// You'll note that the fill ring and completion are a shared liveness requirement but under
/// unique control. Exactly one process has to responsibility of maintaining them and ensuring the
/// rings progress. Failing to do so impacts _all_ sockets sharing this `Umem`. The converse is not
/// true. A single socket can starve its transmission buffer or refuse accepting received packets
/// but the worst is packet loss in this queue.
///
/// The controller of the fill/completion pair also controls the associated bpf program which maps
/// packets onto the set of sockets (aka. 'XSKMAP').
pub struct XskUmem {
    umem_area: NonNull<[u8]>,
    config: XskUmemConfig,
    fd: Arc<SocketFd>,
}

/// One prepared socket for a receive/transmit pair.
///
/// Note: it is not yet _bound_ to a specific `PF_XDP` address (device queue).
pub struct XskSocket {
    info: Arc<IfInfo>,
    fd: Arc<SocketFd>,
}

/// One device queue associated with an XDP socket.
///
/// A socket is more specifically a set of receive and transmit queues for packets (mapping to some
/// underlying hardware mapping those bytes with a network). The fill and completion queue can, in
/// theory, be shared with other sockets of the same `Umem`.
pub struct XskDevice {
    /// Fill and completion queues.
    fcq: XskDeviceRings,
    /// This is also a socket.
    socket: XskSocket,
}

/// A receiver queue.
///
/// This also maintains the mmap of the associated queue.
pub struct XskRxRing {
    ring: XskRingCons,
    fd: Arc<SocketFd>,
}

/// A transmitter queue.
///
/// This also maintains the mmap of the associated queue.
pub struct XskTxRing {
    ring: XskRingProd,
    fd: Arc<SocketFd>,
}

/// A complete (cached) information about a socket.
///
/// Please allocate this, the struct is quite large. Put it into an `Arc` since it is not mutable.
#[derive(Clone, Copy)]
pub struct IfInfo {
    ctx: IfCtx,
    ifname: [libc::c_char; libc::IFNAMSIZ],
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Hash)]
pub struct IfCtx {
    ifindex: libc::c_uint,
    queue_id: u32,
    /// The namespace cookie, associated with a *socket*.
    /// This field is filled by some surrounding struct containing the info.
    netnscookie: u64,
}

pub struct XskDeviceRings {
    pub prod: XskRingProd,
    pub cons: XskRingCons,
    map: SocketMmapOffsets,
}

struct SocketMmapOffsets {
    inner: XdpMmapOffsets,
}

/// An index to an XDP buffer.
///
/// Usually passed from a call of reserved or available buffers(in [`XskRingProd`] and
/// [`XskRingCons`] respectively) to one of the access functions. This resolves the raw index to a
/// memory address in the ring buffer.
///
/// This is _not_ a pure offset, a masking is needed to access the raw offset! The kernel requires
/// the buffer count to be a power-of-two for this to be efficient. Then, producer and consumer
/// heads operate on the 32-bit number range, _silently_ mapping to the same range of indices.
/// (Similar to TCP segments, actually). Well-behaving sides will maintain the order of the two
/// numbers in this wrapping space, which stays perfectly well-defined as long as less than `2**31`
/// buffer are identified in total.
///
/// In other words, you need a configured ring to determine an exact offset or compare two indices.
///
/// This type does _not_ implement comparison traits or hashing! Nevertheless, there's nothing
/// unsafe about creating or observing this detail, so feel free to construct your own or use the
/// transparent layout to (unsafely) treat the type as a `u32` instead.
#[repr(transparent)]
#[derive(Debug, Copy, Clone)]
pub struct BufIdx(pub u32);

/// A producer ring.
///
/// Here, user space maintains the write head and the kernel the read tail.
#[derive(Debug)]
pub struct XskRingProd {
    inner: XskRing,
    mmap_addr: NonNull<[u8]>,
}

/// A consumer ring.
///
/// Here, kernel maintains the write head and user space the read tail.
#[derive(Debug)]
pub struct XskRingCons {
    inner: XskRing,
    mmap_addr: NonNull<[u8]>,
}

impl XskUmem {
    /* Socket options for XDP */
    const XDP_MMAP_OFFSETS: libc::c_int = 1;
    const XDP_RX_RING: libc::c_int = 2;
    const XDP_TX_RING: libc::c_int = 3;
    const XDP_UMEM_REG: libc::c_int = 4;
    const XDP_UMEM_FILL_RING: libc::c_int = 5;
    const XDP_UMEM_COMPLETION_RING: libc::c_int = 6;
    const XDP_STATISTICS: libc::c_int = 7;
    const XDP_OPTIONS: libc::c_int = 8;

    /// Create a new Umem ring.
    ///
    /// # Safety
    ///
    /// The caller passes an area denoting the memory of the ring. It must be valid for the
    /// indicated buffer size and count. The caller is also responsible for keeping the mapping
    /// alive.
    pub unsafe fn new(config: XskUmemConfig, area: NonNull<[u8]>) -> Result<XskUmem, libc::c_int> {
        fn is_page_aligned(area: NonNull<[u8]>) -> bool {
            let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
            // TODO: use `addr()` as we don't need to expose the pointer here. Just the address as
            // an integer and no provenance-preserving cast intended.
            (area.as_ptr() as *mut u8 as usize & (page_size - 1)) == 0
        }

        debug_assert!(
            is_page_aligned(area),
            "UB: Bad mmap area provided, but caller is responsible for its soundness."
        );

        // Two steps:
        // 1. Create a new XDP socket in the kernel.
        // 2. Configure it with the area and size.
        // Safety: correct `socket` call.
        let umem = XskUmem {
            config,
            fd: Arc::new(SocketFd::new()?),
            umem_area: area,
        };

        Self::configure(&umem)?;
        Ok(umem)
    }

    fn configure(this: &XskUmem) -> Result<(), libc::c_int> {
        let mut mr = XdpUmemReg::default();
        mr.addr = this.umem_area.as_ptr() as *mut u8 as u64;
        mr.len = ptr_len(this.umem_area.as_ptr()) as u64;
        mr.chunk_size = this.config.frame_size;
        mr.headroom = this.config.headroom;
        mr.flags = this.config.flags;

        let err = unsafe {
            libc::setsockopt(
                this.fd.0,
                libc::SOL_XDP,
                Self::XDP_UMEM_REG,
                (&mut mr) as *mut _ as *mut libc::c_void,
                core::mem::size_of_val(&mr) as libc::socklen_t,
            )
        };

        if err != 0 {
            return Err(err);
        }

        Ok(())
    }

    /// Map the fill and completion queue of this ring for a device.
    ///
    /// The caller _should_ only call this once for each ring. However, it's not entirely incorrect
    /// to do it multiple times. Just, be careful that the administration becomes extra messy. All
    /// code is written under the assumption that only one controller/writer for the user-space
    /// portions of each queue is active at a time. The kernel won't care about your broken code.
    pub fn fq_cq(&mut self, interface: &XskSocket) -> Result<XskDevice, libc::c_int> {
        todo!()
    }

    /// Configure the device address for a socket.
    ///
    /// Note: if the underlying socket is shared then this will also associate other sockets, this
    /// is intended.
    pub fn bind(
        &mut self,
        interface: &XskSocket,
        sock: &XskSocketConfig,
    ) -> Result<XskSocket, libc::c_int> {
        todo!()
    }
}

impl XskSocket {
    const SO_NETNS_COOKIE: libc::c_int = 71;
    const INIT_NS: u64 = 1;

    pub fn new(interface: &IfInfo) -> Result<Self, libc::c_int> {
        let fd = Arc::new(SocketFd::new()?);
        Self::with_xdp_socket(interface, fd)
    }

    /// Create a socket using the FD of the `umem`.
    ///
    /// # Safety
    ///
    /// It's *not* (memory-)unsafe to run this twice with different interfaces but it's also
    /// incorrect. Please don't.
    pub fn with_shared(interface: &IfInfo, umem: &XskUmem) -> Result<Self, libc::c_int> {
        Self::with_xdp_socket(interface, umem.fd.clone())
    }

    fn with_xdp_socket(interface: &IfInfo, fd: Arc<SocketFd>) -> Result<Self, libc::c_int> {
        let mut info = Arc::new(interface.clone());

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
            err => return Err(err),
        }

        // Won't reallocate in practice.
        Arc::make_mut(&mut info).ctx.netnscookie = netnscookie;

        Ok(XskSocket { fd, info })
    }
}

impl SocketFd {
    fn new() -> Result<Self, libc::c_int> {
        let fd = unsafe { libc::socket(libc::AF_XDP, libc::SOCK_RAW, 0) };
        if fd < 0 {
            return Err(fd);
        }
        Ok(SocketFd(fd))
    }
}

impl Default for XskUmemConfig {
    fn default() -> Self {
        XskUmemConfig {
            fill_size: 1 << 11,
            complete_size: 1 << 11,
            frame_size: 1 << 12,
            headroom: 0,
            flags: 0,
        }
    }
}

impl Drop for SocketFd {
    fn drop(&mut self) {
        let _ = unsafe { libc::close(self.0) };
    }
}

// FIXME: pending stabilization, use pointer::len directly.
// <https://doc.rust-lang.org/stable/std/primitive.pointer.html#method.len>
fn ptr_len(ptr: *mut [u8]) -> usize {
    unsafe { (*(ptr as *mut [()])).len() }
}
