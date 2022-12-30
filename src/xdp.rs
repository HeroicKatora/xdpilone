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

#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct XdpRingOffsets {
    /// the relative address of the producer.
    pub producer: u64,
    /// the relative address of the consumer.
    pub consumer: u64,
    /// the relative address of the descriptor.
    pub desc: u64,
    /// the relative address of the flags area.
    pub flags: u64,
}

/// The offsets as returned by the kernel.
#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct XdpMmapOffsets {
    pub rx: XdpRingOffsets,
    pub tx: XdpRingOffsets,
    /// Fill ring offset.
    pub fr: XdpRingOffsets,
    /// Completion ring offset.
    pub cr: XdpRingOffsets,
}

/// Prior version of XdpMmapOffsets (<= Linux 5.3).
#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct XdpRingOffsetsV1 {
    /// the relative address of the producer.
    pub producer: u64,
    /// the relative address of the consumer.
    pub consumer: u64,
    /// the relative address of the descriptor.
    pub desc: u64,
}

/// Prior version of XdpMmapOffsets (<= Linux 5.3).
#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct XdpMmapOffsetsV1 {
    pub rx: XdpRingOffsetsV1,
    pub tx: XdpRingOffsetsV1,
    pub fr: XdpRingOffsetsV1,
    pub cr: XdpRingOffsetsV1,
}
