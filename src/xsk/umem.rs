use core::ptr::NonNull;

use alloc::collections::BTreeSet;
use alloc::sync::Arc;

use crate::xdp::XdpUmemReg;
use crate::xsk::{
    ptr_len, IfCtx, SocketFd, SocketMmapOffsets, XskDevice, XskDeviceControl, XskDeviceRings,
    XskRingCons, XskRingProd, XskSocket, XskSocketConfig, XskUmem, XskUmemConfig,
};

use spin::RwLock;

impl XskUmem {
    /* Socket options for XDP */
    pub const XDP_MMAP_OFFSETS: libc::c_int = 1;
    pub const XDP_RX_RING: libc::c_int = 2;
    pub const XDP_TX_RING: libc::c_int = 3;
    pub const XDP_UMEM_REG: libc::c_int = 4;
    pub const XDP_UMEM_FILL_RING: libc::c_int = 5;
    pub const XDP_UMEM_COMPLETION_RING: libc::c_int = 6;
    pub const XDP_STATISTICS: libc::c_int = 7;
    pub const XDP_OPTIONS: libc::c_int = 8;

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

        let devices = XskDeviceControl {
            inner: Arc::new(SpinLockedControlSet::default()),
        };

        // Two steps:
        // 1. Create a new XDP socket in the kernel.
        // 2. Configure it with the area and size.
        // Safety: correct `socket` call.
        let umem = XskUmem {
            config,
            fd: Arc::new(SocketFd::new()?),
            umem_area: area,
            devices,
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
        if !self.devices.insert(interface.info.ctx) {
            return Err(libc::EINVAL);
        }

        struct DropableDevice<'info>(&'info IfCtx, &'info XskDeviceControl);

        impl Drop for DropableDevice<'_> {
            fn drop(&mut self) {
                self.1.remove(self.0);
            }
        }

        // Okay, got a device. Let's create the queues for it. On failure, cleanup.
        let _tmp_device = DropableDevice(&interface.info.ctx, &self.devices);

        Self::configure_cq(&*interface.fd, &self.config)?;

        let sock = &*interface.fd;
        let map = SocketMmapOffsets::new(sock.0)?;

        let prod = unsafe { XskRingProd::fill(sock, &map, self.config.fill_size)? };
        let cons = unsafe { XskRingCons::comp(sock, &map, self.config.complete_size)? };

        let device = XskDevice {
            fcq: XskDeviceRings { map, cons, prod },
            socket: XskSocket {
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
    /// Note: if the underlying socket is shared then this will also associate other sockets, this
    /// is intended.
    pub fn bind(
        &mut self,
        interface: &XskSocket,
        sock: &XskSocketConfig,
    ) -> Result<XskSocket, libc::c_int> {
        todo!()
    }

    pub(crate) fn configure_cq(fd: &SocketFd, config: &XskUmemConfig) -> Result<(), libc::c_int> {
        if unsafe {
            libc::setsockopt(
                fd.0,
                libc::SOL_XDP,
                XskUmem::XDP_UMEM_COMPLETION_RING,
                (&config.complete_size) as *const _ as *const libc::c_void,
                core::mem::size_of_val(&config.complete_size) as libc::socklen_t,
            )
        } != 0
        {
            return Err(unsafe { *libc::__errno_location() });
        }

        if unsafe {
            libc::setsockopt(
                fd.0,
                libc::SOL_XDP,
                XskUmem::XDP_UMEM_FILL_RING,
                (&config.fill_size) as *const _ as *const libc::c_void,
                core::mem::size_of_val(&config.fill_size) as libc::socklen_t,
            )
        } != 0
        {
            return Err(unsafe { *libc::__errno_location() });
        }

        todo!()
    }
}

impl XskDevice {
    pub fn setup_xdp_prog(&mut self) -> Result<(), libc::c_int> {
        todo!()
    }
}

#[derive(Default)]
struct SpinLockedControlSet {
    inner: RwLock<BTreeSet<IfCtx>>,
}

impl core::ops::Deref for XskDeviceControl {
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
