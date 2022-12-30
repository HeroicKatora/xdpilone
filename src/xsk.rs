//! Our own XSK (simple XDP ring implementation).
//!
//! Consider: the reasoning behind these structs is their implementation in a _header_ of C code,
//! so that they can be optimized on all platforms. How much sense does it make to not write them
//! in Rust code, where rustc does _exactly_ this.
use crate::xdp::XdpDesc;

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};
use alloc::boxed::Box;

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

/// One device queue associated with an XDP socket.
///
/// A socket is more specifically a set of receive and transmit queues for packets (mapping to some
/// underlying hardware mapping those bytes with a network). The fill and completion queue can, in
/// theory, be shared with other sockets of the same `Umem`.
struct XskSocket {
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

/// The basic Umem descriptor.
///
/// Compared to `libxdp` there is _no_ shared setup at the moment and no link to the queues is
/// stored. Such a struct would necessitate thread-safe access to the ring's producer and consumer
/// queues. This overhead will be represented as separate types and potentially pinned boxes if any
/// reason exists to link the two user-space queues to the `Umem`, i.e. the same memory buffer, by
/// indicating so the kernel. That socket then shares completion and fill rings, but _not_ receive
/// and transmission rings.
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
    umem_area: NonNull<u8>,
    config: XskUmemConfig,
    fd: libc::c_int,
}

struct XdpRingOffsets {
    pub producer: u64,
    pub consumer: u64,
    pub desc: u64,
    pub flags: u64,
}

/// The offsets as returned by the kernel.
struct SocketMmapOffsets {
    pub rx: XdpRingOffsets,
    pub tx: XdpRingOffsets,
    /// Fill ring offset.
    pub fr: XdpRingOffsets,
    /// Completion ring offset.
    pub cr: XdpRingOffsets,
}

pub struct XskRings {
    pub fill: Box<XskRingProd>,
    pub completion: Box<XskRingCons>,
    map: SocketMmapOffsets,
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
}

/// A consumer ring.
///
/// Here, kernel maintains the write head and user space the read tail.
#[derive(Debug)]
pub struct XskRingCons {
    inner: XskRing,
}

impl XskRing {
    /// Construct a ring from an mmap given by the kernel.
    ///
    /// # Safety
    ///
    /// The caller is responsible for ensuring that the memory mapping is valid, and **outlives**
    /// the ring itself. Please attach a reference counted pointer to the controller or something
    /// of that sort.
    ///
    /// The caller must ensure that the memory region is not currently mutably aliased. That's
    /// wrong anyways because the kernel may write to it, i.e. it is not immutable! A shared
    /// aliasing is okay.
    pub unsafe fn new() -> Self {
        todo!()
    }
}

impl XskRingProd {
    pub unsafe fn fill_addr(&self, idx: BufIdx) -> NonNull<u64> {
        let offset = (idx.0 & self.inner.mask) as isize;
        let base = self.inner.ring.cast::<u64>().as_ptr();
        unsafe { NonNull::new_unchecked(base.offset(offset)) }
    }

    pub unsafe fn tx_desc(&self, idx: BufIdx) -> NonNull<XdpDesc> {
        let offset = (idx.0 & self.inner.mask) as isize;
        let base = self.inner.ring.cast::<XdpDesc>().as_ptr();
        unsafe { NonNull::new_unchecked(base.offset(offset)) }
    }

    /// Query for up to `nb` free entries.
    ///
    /// Serves small requests based on cached state about the kernel's consumer head. Large
    /// requests may thus incur an extra refresh of the consumer head.
    pub fn count_free(&mut self, nb: u32) -> u32 {
        let free_entries = self
            .inner
            .cached_consumer
            .wrapping_sub(self.inner.cached_producer);

        if free_entries >= nb {
            return free_entries;
        }

        self.inner.cached_consumer = self.inner.consumer.load(Ordering::Acquire);
        // No-op module the size, but ensures our view of the consumer is always ahead of the
        // producer, no matter buffer counts and mask.
        // TODO: actually, I don't _quite_ understand. This algorithm is copied from libxdp.
        self.inner.cached_consumer += self.inner.size;

        self.inner.cached_consumer - self.inner.cached_producer
    }

    /// Prepare consuming some buffers on our-side, not submitting to the kernel yet.
    ///
    /// Writes the index of the next available buffer into `idx`. Fails if less than the requested
    /// amount of buffers can be reserved.
    pub fn reserve(&mut self, nb: u32, idx: &mut BufIdx) -> u32 {
        if self.count_free(nb) < nb {
            return 0;
        }

        *idx = BufIdx(self.inner.cached_producer);
        self.inner.cached_producer += nb;

        nb
    }

    /// Cancel a previous `reserve`.
    ///
    /// If passed a smaller number, the remaining reservation stays active.
    pub fn cancel(&mut self, nb: u32) {
        self.inner.cached_producer -= nb;
    }

    /// Submit a number of buffers.
    ///
    /// Note: the client side state is _not_ adjusted. If you've called `reserve` before please
    /// check to maintain a consistent view.
    pub fn submit(&mut self, nb: u32) {
        // We are the only writer, all other writes are ordered before.
        let cur = self.inner.producer.load(Ordering::Relaxed);
        // When the kernel reads it, all writes to buffers must be ordered before this write to the
        // head, this represents the memory synchronization edge.
        self.inner
            .producer
            .store(cur.wrapping_add(nb), Ordering::Release);
    }
}

impl XskRingCons {
    pub unsafe fn comp_addr(&self, idx: BufIdx) -> NonNull<u64> {
        let offset = (idx.0 & self.inner.mask) as isize;
        let base = self.inner.ring.cast::<u64>().as_ptr();
        unsafe { NonNull::new_unchecked(base.offset(offset)) }
    }

    pub unsafe fn rx_desc(&self, idx: BufIdx) -> NonNull<XdpDesc> {
        let offset = (idx.0 & self.inner.mask) as isize;
        let base = self.inner.ring.cast::<XdpDesc>().as_ptr();
        unsafe { NonNull::new_unchecked(base.offset(offset)) }
    }

    /// Find the number of available entries.
    pub fn count_available(&mut self, nb: u32) -> u32 {
        let mut available = self
            .inner
            .cached_producer
            .wrapping_sub(self.inner.cached_consumer);

        if available == 0 {
            self.inner.cached_producer = self.inner.producer.load(Ordering::Acquire);
            available = self
                .inner
                .cached_producer
                .wrapping_sub(self.inner.cached_consumer);
        }

        // TODO: huh, this being the only use is pretty weird. Sure, in `peek` we may want to pace
        // our logical consumption of buffers but then it could perform its own `min`? The
        // symmetry to `RingProd` is lost anyways due to the difference in when we refresh our
        // producer cache, right?
        available.min(nb)
    }

    pub fn peek(&mut self, nb: u32, idx: &mut BufIdx) -> u32 {
        let count = self.count_available(nb);

        if count == 0 {
            return 0;
        }

        *idx = BufIdx(self.inner.cached_consumer);
        self.inner.cached_consumer += count;

        count
    }

    /// Cancel a previous `peek`.
    ///
    /// If passed a smaller number, the remaining reservation stays active.
    pub fn cancel(&mut self, nb: u32) {
        self.inner.cached_consumer -= nb;
    }

    pub fn release(&mut self, nb: u32) {
        // We are the only writer, all other writes are ordered before.
        let cur = self.inner.consumer.load(Ordering::Relaxed);
        // All our reads from buffers must be ordered before this write to the head, this
        // represents the memory synchronization edge.
        self.inner
            .consumer
            .store(cur.wrapping_add(nb), Ordering::Release);
    }
}

impl XskUmem {
    /// Create a new Umem ring.
    ///
    /// # Safety
    ///
    /// The caller passes an area denoting the memory of the ring. It must be valid for the
    /// indicated buffer size and count. The caller is also responsible for keeping the mapping
    /// alive.
    pub unsafe fn new(config: XskUmemConfig, area: NonNull<u8>) -> Result<XskUmem, libc::c_int> {
        fn is_page_aligned(area: NonNull<u8>) -> bool {
            let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
            // TODO: use `addr()` as we don't need to expose the pointer here. Just the address as
            // an integer and no provenance-preserving cast intended.
            (area.as_ptr() as usize & (page_size - 1)) == 0
        }

        debug_assert!(
            is_page_aligned(area),
            "UB: Bad mmap area provided, but caller is responsible for its soundness."
        );

        // Two steps:
        // 1. Create a new XDP socket in the kernel.
        // 2. Configure it with the area and size.

        let mut umem = XskUmem {
            config,
            fd: 0,
            umem_area: area,
        };

        // Safety: correct `socket` call.
        umem.fd = unsafe { libc::socket(libc::AF_XDP, libc::SOCK_RAW, 0) };
        if umem.fd < 0 {
            return Err(umem.fd);
        }

        todo!()
    }

    /// Map the fill and completion queue of this socket ring.
    ///
    /// The caller _should_ only call this once for each ring. However, it's not entirely incorrect
    /// to do it multiple times. Just, be careful that the administration becomes extra messy. All
    /// code is written under the assumption that only one controller/writer for the user-space
    /// portions of each queue is active at a time. The kernel won't care about your broken code.
    pub unsafe fn create_fq_cq(&self) -> Result<(Box<XskRingProd>, Box<XskRingCons>), libc::c_int> {
        todo!()
    }

    pub unsafe fn attach_sock_rx_tx(&self) {
        todo!()
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
