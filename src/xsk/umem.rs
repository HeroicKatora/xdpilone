use core::ptr::NonNull;

use alloc::collections::BTreeSet;
use alloc::sync::Arc;

use crate::xdp::{SockAddrXdp, XdpDesc, XdpStatistics, XdpUmemReg};
use crate::xsk::{
    ptr_len, BufIdx, DeviceControl, DeviceQueue, DeviceRings, IfCtx, RingCons, RingProd, RingRx,
    RingTx, Socket, SocketConfig, SocketFd, SocketMmapOffsets, Umem, UmemChunk, UmemConfig, User,
};
use crate::Errno;

use spin::RwLock;

impl BufIdx {
    /// Convert a slice of raw numbers to buffer indices, in-place.
    pub fn from_slice(id: &[u32]) -> &[Self] {
        unsafe { &*(id as *const [u32] as *const [Self]) }
    }

    /// Convert a slice of raw numbers to buffer indices, in-place.
    pub fn from_mut_slice(id: &mut [u32]) -> &mut [Self] {
        unsafe { &mut *(id as *mut [u32] as *mut [Self]) }
    }

    /// Convert a slice buffer indices to raw numbers, in-place.
    pub fn to_slice(this: &[Self]) -> &[u32] {
        unsafe { &*(this as *const [Self] as *const [u32]) }
    }

    /// Convert a slice buffer indices to raw numbers, in-place.
    pub fn to_mut_slice(this: &mut [Self]) -> &mut [u32] {
        unsafe { &mut *(this as *mut [Self] as *mut [u32]) }
    }
}

impl Umem {
    /* Socket options for XDP */
    pub(crate) const XDP_MMAP_OFFSETS: libc::c_int = 1;
    pub(crate) const XDP_RX_RING: libc::c_int = 2;
    pub(crate) const XDP_TX_RING: libc::c_int = 3;
    pub(crate) const XDP_UMEM_REG: libc::c_int = 4;
    pub(crate) const XDP_UMEM_FILL_RING: libc::c_int = 5;
    pub(crate) const XDP_UMEM_COMPLETION_RING: libc::c_int = 6;
    pub(crate) const XDP_STATISTICS: libc::c_int = 7;
    #[allow(dead_code)]
    pub(crate) const XDP_OPTIONS: libc::c_int = 8;

    /// Create a new Umem ring.
    ///
    /// # Safety
    ///
    /// The caller passes an area denoting the memory of the ring. It must be valid for the
    /// indicated buffer size and count. The caller is also responsible for keeping the mapping
    /// alive.
    ///
    /// The area must be page aligned and not exceed i64::MAX in length (on future systems where
    /// you could).
    pub unsafe fn new(config: UmemConfig, area: NonNull<[u8]>) -> Result<Umem, Errno> {
        fn is_page_aligned(area: NonNull<[u8]>) -> bool {
            let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
            // TODO: use `addr()` as we don't need to expose the pointer here. Just the address as
            // an integer and no provenance-preserving cast intended.
            (area.as_ptr() as *mut u8 as usize & (page_size - 1)) == 0
        }

        assert!(config.frame_size > 0, "Invalid frame size");

        assert!(
            is_page_aligned(area),
            "UB: Bad mmap area provided, but caller is responsible for its soundness."
        );

        let area_size = ptr_len(area.as_ptr());

        assert!(
            u64::try_from(area_size).is_ok(),
            "Unhandled address space calculation"
        );

        let devices = DeviceControl {
            inner: Arc::new(SpinLockedControlSet::default()),
        };

        // Two steps:
        // 1. Create a new XDP socket in the kernel.
        // 2. Configure it with the area and size.
        // Safety: correct `socket` call.
        let umem = Umem {
            config,
            fd: Arc::new(SocketFd::new()?),
            umem_area: area,
            devices,
        };

        Self::configure(&umem)?;
        Ok(umem)
    }

    /// Get the address associated with a buffer, if it is in-bounds.
    ///
    /// # Safety
    ///
    /// No requirements. However, please ensure that _use_ of the pointer is done properly. The
    /// pointer is guaranteed to be derived from the `area` passed in the constructor. The method
    /// guarantees that it does not _access_ any of the pointers in this process.
    pub fn frame(&self, idx: BufIdx) -> Option<UmemChunk> {
        let pitch: u32 = self.config.frame_size;
        let idx: u32 = idx.0;
        let area_size = ptr_len(self.umem_area.as_ptr()) as u64;

        // Validate that it fits.
        let offset = u64::from(pitch) * u64::from(idx);
        if area_size.checked_sub(u64::from(pitch)) < Some(offset) {
            return None;
        }

        // Now: area_size is converted, without loss, from an isize that denotes the [u8] length,
        // valid as guaranteed by the caller of the constructor. We have just checked:
        //
        //   `[offset..offset+pitch) < area_size`.
        //
        // So all of the following is within the bounds of the constructor-guaranteed
        // address manipulation.
        let base = unsafe { self.umem_area.cast::<u8>().as_ptr().offset(offset as isize) };
        debug_assert!(!base.is_null(), "UB: offsetting area within produced NULL");
        let slice = core::ptr::slice_from_raw_parts_mut(base, pitch as usize);
        let addr = unsafe { NonNull::new_unchecked(slice) };
        Some(UmemChunk { addr, offset })
    }

    /// Count the number of available data frames.
    pub fn len_frames(&self) -> u32 {
        let area_size = ptr_len(self.umem_area.as_ptr()) as u64;
        let count = area_size / u64::from(self.config.frame_size);
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    fn configure(this: &Umem) -> Result<(), Errno> {
        let mut mr = XdpUmemReg::default();
        mr.addr = this.umem_area.as_ptr() as *mut u8 as u64;
        mr.len = ptr_len(this.umem_area.as_ptr()) as u64;
        mr.chunk_size = this.config.frame_size;
        mr.headroom = this.config.headroom;
        mr.flags = this.config.flags;

        let optlen = core::mem::size_of_val(&mr) as libc::socklen_t;
        let err = unsafe {
            libc::setsockopt(
                this.fd.0,
                super::SOL_XDP,
                Self::XDP_UMEM_REG,
                (&mut mr) as *mut _ as *mut libc::c_void,
                optlen,
            )
        };

        if err != 0 {
            return Err(Errno::new());
        }

        Ok(())
    }

    /// Map the fill and completion queue of this ring for a device.
    ///
    /// The caller _should_ only call this once for each ring. However, it's not entirely incorrect
    /// to do it multiple times. Just, be careful that the administration becomes extra messy. All
    /// code is written under the assumption that only one controller/writer for the user-space
    /// portions of each queue is active at a time. The kernel won't care about your broken code.
    pub fn fq_cq(&self, interface: &Socket) -> Result<DeviceQueue, Errno> {
        if !self.devices.insert(interface.info.ctx) {
            return Err(Errno(libc::EINVAL));
        }

        struct DropableDevice<'info>(&'info IfCtx, &'info DeviceControl);

        impl Drop for DropableDevice<'_> {
            fn drop(&mut self) {
                self.1.remove(self.0);
            }
        }

        // Okay, got a device. Let's create the queues for it. On failure, cleanup.
        let _tmp_device = DropableDevice(&interface.info.ctx, &self.devices);

        let sock = &*interface.fd;
        Self::configure_cq(sock, &self.config)?;
        let map = SocketMmapOffsets::new(sock)?;

        // FIXME: should we be configured the `cached_consumer` and `cached_producer` and
        // potentially other values, here? The setup produces a very rough clone of _just_ the ring
        // itself and none of the logic beyond.
        let prod = unsafe { RingProd::fill(sock, &map, self.config.fill_size) }?;
        let cons = unsafe { RingCons::comp(sock, &map, self.config.complete_size) }?;

        let device = DeviceQueue {
            fcq: DeviceRings { map, cons, prod },
            socket: Socket {
                info: interface.info.clone(),
                fd: interface.fd.clone(),
            },
            devices: self.devices.clone(),
        };

        core::mem::forget(_tmp_device);
        Ok(device)
    }

    /// Configure the device address for a socket.
    ///
    /// Either `rx_size` or `tx_size` must be non-zero, i.e. the call to bind will fail if none of
    /// the rings is actually configured.
    ///
    /// Note: if the underlying socket is shared then this will also bind other objects that share
    /// the underlying socket file descriptor, this is intended.
    pub fn rx_tx(&self, interface: &Socket, config: &SocketConfig) -> Result<User, Errno> {
        let sock = &*interface.fd;
        Self::configure_rt(sock, config)?;
        let map = SocketMmapOffsets::new(sock)?;

        Ok(User {
            socket: Socket {
                info: interface.info.clone(),
                fd: interface.fd.clone(),
            },
            config: Arc::new(config.clone()),
            map,
        })
    }

    /// Activate rx/tx queues by binding the socket to a device.
    ///
    /// Please note that calls to [`User::map_rx`] and [`User::map_tx`] will fail once the
    /// device is bound! Also, the fill and completion queues of the interface/queue must be setup
    /// already.
    pub fn bind(&self, interface: &User) -> Result<(), Errno> {
        let mut sxdp = SockAddrXdp {
            ifindex: interface.socket.info.ctx.ifindex,
            queue_id: interface.socket.info.ctx.queue_id,
            ..SockAddrXdp::default()
        };

        // Note: using a separate socket with shared umem requires one dedicated configured cq for
        // the interface indicated.

        if interface.socket.fd.0 != self.fd.0 {
            sxdp.flags = interface.config.bind_flags | SocketConfig::XDP_BIND_SHARED_UMEM;
            sxdp.shared_umem_fd = self.fd.0 as u32;
        }

        if unsafe {
            libc::bind(
                interface.socket.fd.0,
                (&sxdp) as *const _ as *const libc::sockaddr,
                core::mem::size_of_val(&sxdp) as libc::socklen_t,
            )
        } != 0
        {
            return Err(Errno::new());
        }

        Ok(())
    }

    pub(crate) fn configure_cq(fd: &SocketFd, config: &UmemConfig) -> Result<(), Errno> {
        if unsafe {
            libc::setsockopt(
                fd.0,
                super::SOL_XDP,
                Umem::XDP_UMEM_COMPLETION_RING,
                (&config.complete_size) as *const _ as *const libc::c_void,
                core::mem::size_of_val(&config.complete_size) as libc::socklen_t,
            )
        } != 0
        {
            return Err(Errno::new());
        }

        if unsafe {
            libc::setsockopt(
                fd.0,
                super::SOL_XDP,
                Umem::XDP_UMEM_FILL_RING,
                (&config.fill_size) as *const _ as *const libc::c_void,
                core::mem::size_of_val(&config.fill_size) as libc::socklen_t,
            )
        } != 0
        {
            return Err(Errno::new());
        }

        Ok(())
    }

    pub(crate) fn configure_rt(fd: &SocketFd, config: &SocketConfig) -> Result<(), Errno> {
        if let Some(num) = config.rx_size {
            if unsafe {
                libc::setsockopt(
                    fd.0,
                    super::SOL_XDP,
                    Umem::XDP_RX_RING,
                    (&num) as *const _ as *const libc::c_void,
                    core::mem::size_of_val(&num) as libc::socklen_t,
                )
            } != 0
            {
                return Err(Errno::new());
            }
        }

        if let Some(num) = config.tx_size {
            if unsafe {
                libc::setsockopt(
                    fd.0,
                    super::SOL_XDP,
                    Umem::XDP_TX_RING,
                    (&num) as *const _ as *const libc::c_void,
                    core::mem::size_of_val(&num) as libc::socklen_t,
                )
            } != 0
            {
                return Err(Errno::new());
            }
        }

        Ok(())
    }
}

impl DeviceQueue {
    /// Get the statistics of this XDP socket.
    pub fn statistics(&self) -> Result<XdpStatistics, Errno> {
        XdpStatistics::new(&*self.socket.fd)
    }

    /// Configure a default XDP program.
    ///
    /// This is necessary to start receiving packets on any of the related receive rings, i.e. to
    /// start consuming from the fill queue and fill the completion queue.
    pub fn setup_xdp_prog(&mut self) -> Result<(), libc::c_int> {
        todo!()
    }
}

impl User {
    /// Get the statistics of this XDP socket.
    pub fn statistics(&self) -> Result<XdpStatistics, Errno> {
        XdpStatistics::new(&*self.socket.fd)
    }

    /// Map the RX ring into memory, returning a handle.
    ///
    /// Fails if you did not pass any size for `rx_size` in the configuration, which should be somewhat obvious.
    ///
    /// FIXME: we allow mapping the ring more than once. Not a memory safety problem afaik, but a
    /// correctness problem.
    pub fn map_rx(&self) -> Result<RingRx, Errno> {
        let rx_size = self.config.rx_size.ok_or(Errno(-libc::EINVAL))?.get();
        let ring = unsafe { RingCons::rx(&self.socket.fd, &self.map, rx_size) }?;
        Ok(RingRx {
            fd: self.socket.fd.clone(),
            ring,
        })
    }

    /// Map the TX ring into memory, returning a handle.
    ///
    /// Fails if you did not pass any size for `tx_size` in the configuration, which should be somewhat obvious.
    ///
    /// FIXME: we allow mapping the ring more than once. Not a memory safety problem afaik, but a
    /// correctness problem.
    pub fn map_tx(&self) -> Result<RingTx, Errno> {
        let tx_size = self.config.tx_size.ok_or(Errno(-libc::EINVAL))?.get();
        let ring = unsafe { RingProd::tx(&self.socket.fd, &self.map, tx_size) }?;
        Ok(RingTx {
            fd: self.socket.fd.clone(),
            ring,
        })
    }
}

impl SocketConfig {
    /// Flag-bit for [`Umem::bind`] that the descriptor is shared.
    ///
    /// Generally, this flag need not be passed directly. Instead, it is set within by the library
    /// when the same `Umem` is used for multiple interface/queue combinations.
    pub const XDP_BIND_SHARED_UMEM: u16 = 1 << 0;
    /// Force copy-mode.
    pub const XDP_BIND_COPY: u16 = 1 << 1;
    /// Force zero-copy-mode.
    pub const XDP_BIND_ZEROCOPY: u16 = 1 << 2;
    /// Enable support for need wakeup.
    ///
    /// Needs to be set for [`DeviceQueue::needs_wakeup`] and [`RingTx::needs_wakeup`].
    pub const XDP_BIND_NEED_WAKEUP: u16 = 1 << 3;
}

#[derive(Default)]
struct SpinLockedControlSet {
    inner: RwLock<BTreeSet<IfCtx>>,
}

impl core::ops::Deref for DeviceControl {
    type Target = dyn super::ControlSet;
    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}

impl super::ControlSet for SpinLockedControlSet {
    fn insert(&self, ctx: IfCtx) -> bool {
        let mut lock = self.inner.write();
        lock.insert(ctx)
    }

    fn contains(&self, ctx: &IfCtx) -> bool {
        let lock = self.inner.read();
        lock.contains(ctx)
    }

    fn remove(&self, ctx: &IfCtx) {
        let mut lock = self.inner.write();
        lock.remove(ctx);
    }
}

impl UmemChunk {
    /// Turn this whole chunk into a concrete descriptor for the transmit ring.
    ///
    /// If you've the address or offset are not as returned by the ring then the result is
    /// unspecified, but sound. And potentially safe to use, but the kernel may complain.
    pub fn as_xdp(self) -> XdpDesc {
        let len = ptr_len(self.addr.as_ptr()) as u32;
        self.as_xdp_with_len(len)
    }

    /// Turn into a descriptor with concrete length.
    ///
    /// # Panics
    ///
    /// When debug assertions are enabled, this panics if the length is longer than the address
    /// range refers to.
    pub fn as_xdp_with_len(self, len: u32) -> XdpDesc {
        debug_assert!(
            len <= ptr_len(self.addr.as_ptr()) as u32,
            "Invalid XDP descriptor length {} for chunk of size {}",
            len,
            ptr_len(self.addr.as_ptr()) as u32,
        );

        XdpDesc {
            addr: self.offset,
            len,
            options: 0,
        }
    }
}
