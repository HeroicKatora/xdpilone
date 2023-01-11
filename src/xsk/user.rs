use crate::xdp::XdpDesc;
use crate::xsk::{BufIdx, XskDeviceQueue, XskRingCons, XskRingProd, XskRxRing, XskTxRing, XskUser};

impl XskDeviceQueue {
    /// Add some buffers to the fill ring.
    pub fn fill(&mut self, n: u32) -> WriteFill<'_> {
        WriteFill {
            idx: BufIdxIter::reserve(&mut self.fcq.prod, n),
            queue: &mut self.fcq.prod,
        }
    }

    /// Reap some buffers from the completion ring.
    pub fn complete(&mut self, n: u32) -> ReadComplete<'_> {
        ReadComplete {
            idx: BufIdxIter::peek(&mut self.fcq.cons, n),
            queue: &mut self.fcq.cons,
        }
    }
}

impl Drop for XskDeviceQueue {
    fn drop(&mut self) {
        self.devices.remove(&self.socket.info.ctx);
    }
}

impl XskUser {}

impl XskRxRing {
    /// Receive some buffers.
    ///
    /// Returns an iterator over the descriptors.
    pub fn receive(&mut self, n: u32) -> ReadRx<'_> {
        ReadRx {
            idx: BufIdxIter::peek(&mut self.ring, n),
            queue: &mut self.ring,
        }
    }
}

impl XskTxRing {
    const XDP_RING_NEED_WAKEUP: u32 = 1 << 0;

    /// Transmit some buffers.
    ///
    /// Returns a proxy that can be fed descriptors.
    pub fn transmit(&mut self, n: u32) -> WriteTx<'_> {
        WriteTx {
            idx: BufIdxIter::reserve(&mut self.ring, n),
            queue: &mut self.ring,
        }
    }

    pub fn needs_wakeup(&self) -> bool {
        self.ring.check_flags() & Self::XDP_RING_NEED_WAKEUP != 0
    }

    pub fn wake(&self) {
        let _ = unsafe {
            libc::sendto(
                self.fd.0,
                core::ptr::null_mut(),
                0,
                libc::MSG_DONTWAIT,
                core::ptr::null_mut(),
                0,
            )
        };
    }
}

struct BufIdxIter {
    /// The base of our operation.
    base: BufIdx,
    /// The number of peeked buffers.
    buffers: u32,
    /// The number of buffers still left.
    remain: u32,
}

/// A writer to a fill queue.
pub struct WriteFill<'queue> {
    idx: BufIdxIter,
    /// The queue we read from.
    queue: &'queue mut XskRingProd,
}

/// A reader from a completion queue.
pub struct ReadComplete<'queue> {
    idx: BufIdxIter,
    /// The queue we read from.
    queue: &'queue mut XskRingCons,
}

/// A writer to a transmission (TX) queue.
pub struct WriteTx<'queue> {
    idx: BufIdxIter,
    /// The queue we read from.
    queue: &'queue mut XskRingProd,
}

/// A reader from an receive (RX) queue.
pub struct ReadRx<'queue> {
    idx: BufIdxIter,
    /// The queue we read from.
    queue: &'queue mut XskRingCons,
}

impl Iterator for BufIdxIter {
    type Item = BufIdx;
    fn next(&mut self) -> Option<BufIdx> {
        let next = self.remain.checked_sub(1)?;
        self.remain = next;
        let ret = self.base;
        self.base.0 = self.base.0.wrapping_add(1);
        Some(ret)
    }
}

impl BufIdxIter {
    fn peek(queue: &mut XskRingCons, n: u32) -> Self {
        let mut this = BufIdxIter {
            buffers: 0,
            remain: 0,
            base: BufIdx(0),
        };
        this.buffers = queue.peek(1..=n, &mut this.base);
        this.remain = this.buffers;
        this
    }

    fn reserve(queue: &mut XskRingProd, n: u32) -> Self {
        let mut this = BufIdxIter {
            buffers: 0,
            remain: 0,
            base: BufIdx(0),
        };
        this.buffers = queue.reserve(1..=n, &mut this.base);
        this.remain = this.buffers;
        this
    }

    fn commit_prod(&mut self, queue: &mut XskRingProd) {
        // This contains an atomic write, which LLVM won't even try to optimize away.
        // But, as long as queues are filled there's a decent chance that we didn't manage to
        // reserve or fill a single buffer.
        //
        // FIXME: Should we expose this as a hint to the user? I.e. `commit_likely_empty` with a
        // hint. As well as better ways to avoid doing any work at all.
        if self.buffers > 0 {
            let count = self.buffers - self.remain;
            queue.submit(count);
            self.buffers -= count;
            self.base.0 += count;
        }
    }

    fn release_cons(&mut self, queue: &mut XskRingCons) {
        // See also `commit_prod`.
        if self.buffers > 0 {
            let count = self.buffers - self.remain;
            queue.release(count);
            self.buffers -= count;
            self.base.0 += count;
        }
    }
}

impl WriteFill<'_> {
    /// The total number of available slots.
    pub fn capacity(&self) -> u32 {
        self.idx.buffers
    }

    /// Fill additional slots that were reserved.
    ///
    /// The iterator is polled only for each available slot until either is empty. Returns the
    /// total number of slots filled.
    pub fn insert(&mut self, it: impl Iterator<Item = u64>) -> u32 {
        let mut n = 0;
        for (bufidx, item) in self.idx.by_ref().zip(it) {
            n += 1;
            unsafe { *self.queue.fill_addr(bufidx).as_ptr() = item };
        }
        n
    }

    /// Commit the previously written buffers to the kernel.
    pub fn commit(&mut self) {
        self.idx.commit_prod(self.queue)
    }
}

impl Drop for WriteFill<'_> {
    fn drop(&mut self) {
        // Unless everything is committed, roll back the cached queue state.
        if self.idx.buffers != 0 {
            self.queue.cancel(self.idx.buffers)
        }
    }
}

impl ReadComplete<'_> {
    /// The total number of available buffers.
    pub fn capacity(&self) -> u32 {
        self.idx.buffers
    }

    pub fn read(&mut self) -> Option<XdpDesc> {
        let bufidx = self.idx.next()?;
        // Safety: the buffer is from that same queue by construction.
        Some(unsafe { *self.queue.rx_desc(bufidx).as_ptr() })
    }

    /// Commit some of the written buffers to the kernel.
    pub fn release(&mut self) {
        self.idx.release_cons(self.queue)
    }
}

impl Drop for ReadComplete<'_> {
    fn drop(&mut self) {
        // Unless everything is committed, roll back the cached queue state.
        if self.idx.buffers != 0 {
            self.queue.cancel(self.idx.buffers)
        }
    }
}

impl WriteTx<'_> {
    /// The total number of available slots.
    pub fn capacity(&self) -> u32 {
        self.idx.buffers
    }

    pub fn insert(&mut self, it: impl Iterator<Item = XdpDesc>) -> u32 {
        let mut n = 0;
        for (bufidx, item) in self.idx.by_ref().zip(it) {
            n += 1;
            unsafe { *self.queue.tx_desc(bufidx).as_ptr() = item };
        }
        n
    }

    /// Commit the previously written buffers to the kernel.
    pub fn commit(&mut self) {
        self.idx.commit_prod(self.queue);
    }
}

impl Drop for WriteTx<'_> {
    fn drop(&mut self) {
        // Unless everything is committed, roll back the cached queue state.
        if self.idx.buffers != 0 {
            self.queue.cancel(self.idx.buffers)
        }
    }
}

impl ReadRx<'_> {
    /// The total number of available buffers.
    pub fn capacity(&self) -> u32 {
        self.idx.buffers
    }

    pub fn read(&mut self) -> Option<u64> {
        let bufidx = self.idx.next()?;
        // Safety: the buffer is from that same queue by construction.
        Some(unsafe { *self.queue.comp_addr(bufidx).as_ptr() })
    }

    /// Commit some of the written buffers to the kernel.
    pub fn release(&mut self) {
        self.idx.release_cons(self.queue)
    }
}

impl Drop for ReadRx<'_> {
    fn drop(&mut self) {
        // Unless everything is committed, roll back the cached queue state.
        if self.idx.buffers != 0 {
            self.queue.cancel(self.idx.buffers)
        }
    }
}
