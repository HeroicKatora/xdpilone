//! Our own XSK (user-space XDP ring implementation).
//!
//! Consider: the reasoning behind these structs is their implementation in a _header_ of C code,
//! so that they can be optimized on all platforms. How much sense does it make to not write them
//! in Rust code, where rustc does _exactly_ this.
//!
//! The data structures here are not *safe* to construct. Some of them depend on the caller to
//! uphold guarantees such as keeping an mmap alive, or holding onto a socket for them. Take care.

/// Implementations for interface related operations.
mod iface;
/// Implementations for primitives `XskRing`, `XskRingProd`, `XskRingCons`.
mod ring;
/// Implementations for sockets.
mod socket;
/// Implementation for memory management.
mod umem;
/// Implementations for the actual queue management (user-space side).
mod user;

use crate::xdp::XdpMmapOffsets;

use alloc::sync::Arc;
use core::sync::atomic::AtomicU32;
use core::{num::NonZeroU32, ptr::NonNull};

pub(crate) struct SocketFd(libc::c_int);

/// Not defined in all libc versions and a _system_ property, not an implementation property. Thus
/// we define it ourselves here.
pub(crate) const SOL_XDP: libc::c_int = 283;

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

#[derive(Debug, Clone)]
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

#[derive(Debug, Default, Clone)]
pub struct XskSocketConfig {
    pub rx_size: Option<NonZeroU32>,
    pub tx_size: Option<NonZeroU32>,
    /// Flags to pass on to libxdp/libbpf.
    /// FIXME: but we're not using them?
    #[allow(dead_code)]
    pub lib_flags: u32,
    pub xdp_flags: u32,
    pub bind_flags: u16,
}

/// The basic Umem descriptor.
///
/// This struct manages the buffers themselves, in a high-level sense, not any of the
/// communication or queues.
///
/// Compared to `libxdp` there no link to the queues is stored. Such a struct would necessitate
/// thread-safe access to the ring's producer and consumer queues. Instead, a `XskDeviceQueue` is the
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
// Implementation: <xsk/umem.rs>
pub struct XskUmem {
    umem_area: NonNull<[u8]>,
    config: XskUmemConfig,
    fd: Arc<SocketFd>,
    devices: XskDeviceControl,
}

#[derive(Clone)]
struct XskDeviceControl {
    /// The tracker, not critical for memory safety (here anyways) but correctness.
    inner: Arc<dyn ControlSet>,
}

/// A synchronized set for tracking which `IfCtx` are taken.
trait ControlSet: Send + Sync + 'static {
    fn insert(&self, _: IfCtx) -> bool;
    fn contains(&self, _: &IfCtx) -> bool;
    fn remove(&self, _: &IfCtx);
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
pub struct XskDeviceQueue {
    /// Fill and completion queues.
    fcq: XskDeviceRings,
    /// This is also a socket.
    socket: XskSocket,
    /// Reference to de-register.
    devices: XskDeviceControl,
}

/// An owner of receive/transmit queues.
///
/// This represents a _bound_ and configured version of the raw `XskSocket`.
///
/// FIXME: name is somewhat suboptimal?
pub struct XskUser {
    /// A clone of the socket it was created from.
    socket: XskSocket,
    /// The configuration with which it was created.
    config: Arc<XskSocketConfig>,
    /// A cached version of the map describing receive/tranmit queues.
    map: SocketMmapOffsets,
}

/// A receiver queue.
///
/// This also maintains the mmap of the associated queue.
// Implemented in <xsk/user.rs>
pub struct XskRxRing {
    ring: XskRingCons,
    fd: Arc<SocketFd>,
}

/// A transmitter queue.
///
/// This also maintains the mmap of the associated queue.
// Implemented in <xsk/user.rs>
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

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
    pub(crate) map: SocketMmapOffsets,
}

#[derive(Debug)]
pub(crate) struct SocketMmapOffsets {
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
