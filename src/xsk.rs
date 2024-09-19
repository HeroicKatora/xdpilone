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
/// Implementations for primitives `XskRing`, `RingProd`, `RingCons`.
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

pub use self::user::{ReadComplete, ReadRx, WriteFill, WriteTx};

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

/// Static configuration describing a memory area to use for ring chunks.
#[derive(Debug, Clone)]
pub struct UmemConfig {
    /// Number of entries in the fill queue.
    pub fill_size: u32,
    /// Number of entries in the completion queue.
    pub complete_size: u32,
    /// Size of data chunks in each of the ring queues.
    pub frame_size: u32,
    /// Reserved area at the start of the kernel area.
    pub headroom: u32,
    /// Flags to set with the creation calls.
    pub flags: u32,
}

/// Configuration for a created socket.
///
/// Passed to [`Umem::rx_tx`]
#[derive(Debug, Default, Clone)]
pub struct SocketConfig {
    /// The number of receive descriptors in the ring.
    pub rx_size: Option<NonZeroU32>,
    /// The number of transmit descriptors in the ring.
    pub tx_size: Option<NonZeroU32>,
    /// Additional flags to pass to the `bind` call as part of `sockaddr_xdp`.
    pub bind_flags: u16,
}

/// The basic Umem descriptor.
///
/// This struct manages the buffers themselves, in a high-level sense, not any of the
/// communication or queues.
///
/// Compared to `libxdp` there no link to the queues is stored. Such a struct would necessitate
/// thread-safe access to the ring's producer and consumer queues. Instead, a `DeviceQueue` is the
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
pub struct Umem {
    umem_area: NonNull<[u8]>,
    config: UmemConfig,
    fd: Arc<SocketFd>,
    devices: DeviceControl,
}

/// A raw pointer to a specific chunk in a Umem.
///
/// It's unsafe to access the frame, by design. All aspects of _managing_ the contents of the
/// kernel-shared memory are left to the user of the library.
#[derive(Clone, Copy, Debug)]
pub struct UmemChunk {
    /// The address range associated with the chunk.
    pub addr: NonNull<[u8]>,
    /// The absolute offset of this chunk from the start of the Umem.
    ///
    /// This is the basis of the address calculation shared with the kernel.
    pub offset: u64,
}

#[derive(Clone)]
struct DeviceControl {
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
pub struct Socket {
    info: Arc<IfInfo>,
    fd: Arc<SocketFd>,
}

/// One device queue associated with an XDP socket.
///
/// A socket is more specifically a set of receive and transmit queues for packets (mapping to some
/// underlying hardware mapping those bytes with a network). The fill and completion queue can, in
/// theory, be shared with other sockets of the same `Umem`.
pub struct DeviceQueue {
    /// Fill and completion queues.
    fcq: DeviceRings,
    /// This is also a socket.
    socket: Socket,
    /// Reference to de-register.
    devices: DeviceControl,
}

/// An owner of receive/transmit queues.
///
/// This represents a configured version of the raw `Socket`. It allows you to map the required
/// rings and _then_ [`Umem::bind`] the socket, enabling the operations of the queues with the
/// interface.
pub struct User {
    /// A clone of the socket it was created from.
    socket: Socket,
    /// The configuration with which it was created.
    config: Arc<SocketConfig>,
    /// A cached version of the map describing receive/tranmit queues.
    map: SocketMmapOffsets,
}

/// A receiver queue.
///
/// This also maintains the mmap of the associated queue.
// Implemented in <xsk/user.rs>
pub struct RingRx {
    ring: RingCons,
    fd: Arc<SocketFd>,
}

/// A transmitter queue.
///
/// This also maintains the mmap of the associated queue.
// Implemented in <xsk/user.rs>
pub struct RingTx {
    ring: RingProd,
    fd: Arc<SocketFd>,
}

/// A complete (cached) information about a socket.
///
/// Please allocate this, the struct is quite large. For instance, put it into an `Arc` as soon as
/// it is no longer mutable, or initialize it in-place with [`Arc::get_mut`].
#[derive(Clone, Copy)]
pub struct IfInfo {
    ctx: IfCtx,
    ifname: [libc::c_char; libc::IFNAMSIZ],
}

/// Reduced version of `IfCtx`, only retaining numeric IDs for the kernel.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct IfCtx {
    ifindex: libc::c_uint,
    queue_id: u32,
    /// The namespace cookie, associated with a *socket*.
    /// This field is filled by some surrounding struct containing the info.
    netnscookie: u64,
}

pub(crate) struct DeviceRings {
    pub prod: RingProd,
    pub cons: RingCons,
    // Proof that we obtained this. Not sure if and where we'd use it.
    #[allow(dead_code)]
    pub(crate) map: SocketMmapOffsets,
}

#[derive(Debug)]
pub(crate) struct SocketMmapOffsets {
    inner: XdpMmapOffsets,
}

/// An index to an XDP buffer.
///
/// Usually passed from a call of reserved or available buffers(in [`RingProd`] and
/// [`RingCons`] respectively) to one of the access functions. This resolves the raw index to a
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
pub struct RingProd {
    inner: XskRing,
    mmap_addr: NonNull<[u8]>,
}

/// A consumer ring.
///
/// Here, kernel maintains the write head and user space the read tail.
#[derive(Debug)]
pub struct RingCons {
    inner: XskRing,
    mmap_addr: NonNull<[u8]>,
}

impl Default for UmemConfig {
    fn default() -> Self {
        UmemConfig {
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
//
// FIXME: In 1.79 this was stabilized. Bump MSRV fine?
fn ptr_len(ptr: *mut [u8]) -> usize {
    unsafe { (*(ptr as *mut [()])).len() }
}

impl Socket {
    /// Get the raw file descriptor number underlying this socket.
    pub fn as_raw_fd(&self) -> i32 {
        self.fd.0
    }
}

impl User {
    /// Get the raw file descriptor number underlying this socket.
    pub fn as_raw_fd(&self) -> i32 {
        self.socket.as_raw_fd()
    }
}
