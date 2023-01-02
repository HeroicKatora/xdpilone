use core::{ptr::NonNull, ops::RangeInclusive};
use core::sync::atomic::Ordering;

use crate::Errno;
use crate::xdp::{XdpDesc, XdpRingOffsets};
use crate::xsk::{BufIdx, SocketFd, SocketMmapOffsets, XskRing, XskRingCons, XskRingProd};

impl XskRing {
    const XDP_PGOFF_RX_RING: libc::off_t = 0;
    const XDP_PGOFF_TX_RING: libc::off_t = 0x80000000;
    const XDP_UMEM_PGOFF_FILL_RING: libc::off_t = 0x100000000;
    const XDP_UMEM_PGOFF_COMPLETION_RING: libc::off_t = 0x180000000;

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
    pub unsafe fn new(tx_map: NonNull<u8>, off: &XdpRingOffsets, count: u32) -> Self {
        debug_assert!(count.is_power_of_two());
        let tx_map: *mut u8 = tx_map.as_ptr();
        let trust_offset = |off: u64| NonNull::new_unchecked(tx_map.offset(off as isize));

        let producer = trust_offset(off.producer).cast().as_ref();
        let consumer = trust_offset(off.consumer).cast().as_ref();

        let ring = trust_offset(off.desc).cast();
        let flags = trust_offset(off.flags).cast();

        XskRing {
            mask: count - 1,
            size: count,
            producer,
            consumer,
            ring,
            flags,
            cached_producer: producer.load(Ordering::Relaxed),
            cached_consumer: consumer.load(Ordering::Relaxed),
        }
    }

    unsafe fn map(
        fd: &SocketFd,
        off: &XdpRingOffsets,
        count: u32,
        sz: u64,
        offset: libc::off_t,
    ) -> Result<(Self, NonNull<[u8]>), Errno> {
        let len = (off.desc + u64::from(count) * sz) as usize;

        let mmap = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_POPULATE,
                fd.0,
                offset,
            )
        };

        if mmap == libc::MAP_FAILED {
            return Err(Errno::new());
        }

        assert!(!mmap.is_null());
        // Safety: as by MMap this pointer is valid.
        let mmap_addr = core::ptr::slice_from_raw_parts_mut(mmap as *mut u8, len);
        let mmap_addr = unsafe { NonNull::new_unchecked(mmap_addr) };
        let nn = mmap_addr.cast();

        Ok((XskRing::new(nn, off, count), mmap_addr))
    }
}

impl XskRingProd {
    /// # Safety
    ///
    /// The caller must only pass `fd` and `off` if they correspond as they were returned by the
    /// kernel.
    pub(crate) unsafe fn fill(
        fd: &SocketFd,
        off: &SocketMmapOffsets,
        count: u32,
    ) -> Result<Self, Errno> {
        let (inner, mmap_addr) = XskRing::map(
            fd,
            &off.inner.fr,
            count,
            core::mem::size_of::<u64>() as u64,
            XskRing::XDP_UMEM_PGOFF_FILL_RING,
        )?;

        Ok(XskRingProd { inner, mmap_addr })
    }

    /// # Safety
    ///
    /// The caller must only pass `fd` and `off` if they correspond as they were returned by the
    /// kernel.
    pub(crate) unsafe fn tx(
        fd: &SocketFd,
        off: &SocketMmapOffsets,
        count: u32,
    ) -> Result<Self, Errno> {
        let (inner, mmap_addr) = XskRing::map(
            fd,
            &off.inner.tx,
            count,
            core::mem::size_of::<XdpDesc>() as u64,
            XskRing::XDP_PGOFF_TX_RING,
        )?;

        Ok(XskRingProd { inner, mmap_addr })
    }

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
    pub fn count_free(&mut self, mininmum: u32) -> u32 {
        let free_entries = self
            .inner
            .cached_consumer
            .wrapping_sub(self.inner.cached_producer);

        if free_entries >= mininmum {
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
    /// amount of buffers can be reserved. Returns the number of actual buffers reserved.
    pub fn reserve(&mut self, nb: RangeInclusive<u32>, idx: &mut BufIdx) -> u32 {
        let (start, end) = (*nb.start(), *nb.end());
        let free = self.count_free(start);

        if free < start {
            return 0;
        }

        let free = free.min(end);
        *idx = BufIdx(self.inner.cached_producer);
        self.inner.cached_producer += free;

        free
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
    ///
    /// TODO: interestingly this could be implemented on a shared reference. But is doing so
    /// useful? There's no affirmation that the _intended_ buffers are submitted.
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
    /// Create a completion ring.
    /// # Safety
    ///
    /// The caller must only pass `fd` and `off` if they correspond as they were returned by the
    /// kernel.
    pub(crate) unsafe fn comp(
        fd: &SocketFd,
        off: &SocketMmapOffsets,
        count: u32,
    ) -> Result<Self, Errno> {
        let (inner, mmap_addr) = XskRing::map(
            fd,
            &off.inner.cr,
            count,
            core::mem::size_of::<u64>() as u64,
            XskRing::XDP_UMEM_PGOFF_COMPLETION_RING,
        )?;

        Ok(XskRingCons { inner, mmap_addr })
    }

    /// Create a receive ring.
    /// # Safety
    ///
    /// The caller must only pass `fd` and `off` if they correspond as they were returned by the
    /// kernel.
    pub(crate) unsafe fn rx(
        fd: &SocketFd,
        off: &SocketMmapOffsets,
        count: u32,
    ) -> Result<Self, Errno> {
        let (inner, mmap_addr) = XskRing::map(
            fd,
            &off.inner.rx,
            count,
            core::mem::size_of::<XdpDesc>() as u64,
            XskRing::XDP_PGOFF_RX_RING,
        )?;

        Ok(XskRingCons { inner, mmap_addr })
    }
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
    ///
    /// Any count lower than `expected` will try to refresh the consumer.
    pub fn count_available(&mut self, expected: u32) -> u32 {
        let mut available = self
            .inner
            .cached_producer
            .wrapping_sub(self.inner.cached_consumer);

        if available < expected {
            let new_val = self.inner.producer.load(Ordering::Relaxed);
            available = new_val.wrapping_sub(self.inner.cached_consumer);
            self.inner.cached_producer = self.inner.producer.load(Ordering::Acquire);
        }

        available
    }

    pub fn peek(&mut self, nb: RangeInclusive<u32>, idx: &mut BufIdx) -> u32 {
        let (start, end) = (*nb.start(), *nb.end());
        let count = self.count_available(start);

        if count < start {
            return 0;
        }

        let count = count.min(end);
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

    /// Mark some buffers as processed.
    ///
    /// TODO: interestingly this could be implemented on a shared reference. But is doing so
    /// useful? There's no affirmation that the _intended_ buffers are submitted.
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

impl Drop for XskRingProd {
    fn drop(&mut self) {
        let len = super::ptr_len(self.mmap_addr.as_ptr());
        unsafe { libc::munmap(self.mmap_addr.as_ptr() as *mut _, len) };
    }
}

impl Drop for XskRingCons {
    fn drop(&mut self) {
        let len = super::ptr_len(self.mmap_addr.as_ptr());
        unsafe { libc::munmap(self.mmap_addr.as_ptr() as *mut _, len) };
    }
}
