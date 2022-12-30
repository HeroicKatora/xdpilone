/// Rx/Tx descriptor.
///
/// The layout of this struct is part of the kernel interface.
#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct XdpDesc {
    pub addr: u64,
    pub len: u32,
    pub options: u32,
}

/// Argument to `setsockopt(_, SOL_XDP, XDP_UMEM_REG)`.
#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct XdpUmemReg {
    pub addr: u64,
    pub len: u64,
    pub chunk_size: u32,
    pub headroom: u32,
    pub flags: u32,
}
