// Please see the respective Linux documentation instead.
#![allow(missing_docs)]

/// Rx/Tx descriptor.
///
/// The layout of this struct is part of the kernel interface.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone)]
pub struct XdpDesc {
    /// Full address of this descriptor.
    pub addr: u64,
    /// Logical length of the buffer referenced by the descriptor.
    pub len: u32,
    /// A bitfield of options.
    pub options: u32,
}

/// Argument to `setsockopt(_, SOL_XDP, XDP_UMEM_REG)`.
///
/// Note that this struct's size determines the kernel interpretation of the option. In particular,
/// padding passes garbage to the kernel while indicating said garbage as values!
#[repr(C)]
#[derive(Default, Debug, Copy, Clone)]
pub struct XdpUmemReg {
    pub addr: u64,
    pub len: u64,
    pub chunk_size: u32,
    pub headroom: u32,
    pub flags: u32,
    pub tx_metadata_len: u32,
}

const _NO_PADDING: () = {
    assert!(
        core::mem::size_of::<XdpUmemReg>()
        // For each field. Keep in sync.
            == (core::mem::size_of::<u64>()
                + core::mem::size_of::<u64>()
                + core::mem::size_of::<u32>()
                + core::mem::size_of::<u32>()
                + core::mem::size_of::<u32>()
                + core::mem::size_of::<u32>())
    );
};

/// The mmap-offsets to use for mapping one ring of an XDP socket.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone)]
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

/// The different offsets as returned by the kernel, for all rings of a socket.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone)]
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
#[derive(Default, Debug, Copy, Clone)]
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
#[derive(Default, Debug, Copy, Clone)]
pub struct XdpMmapOffsetsV1 {
    /// Offsets for the receive ring (kernel produced).
    pub rx: XdpRingOffsetsV1,
    /// Offsets for the transmit ring (user produced).
    pub tx: XdpRingOffsetsV1,
    /// Offsets for the fill ring (user produced).
    pub fr: XdpRingOffsetsV1,
    /// Offsets for the completion ring (kernel produced).
    pub cr: XdpRingOffsetsV1,
}

#[repr(C)]
#[doc(alias = "sockaddr_xdp")]
#[derive(Debug, Copy, Clone)]
pub struct SockAddrXdp {
    #[doc(alias = "sxdp_family")]
    pub family: u16,
    #[doc(alias = "sxdp_flags")]
    pub flags: u16,
    #[doc(alias = "sxdp_ifindex")]
    pub ifindex: u32,
    #[doc(alias = "sxdp_queue_id")]
    pub queue_id: u32,
    #[doc(alias = "sxdp_shared_umem_fd")]
    pub shared_umem_fd: u32,
}

/// Prior version of XdpStatisticsV2 that only contains fields present from <= Linux 5.8
#[repr(C)]
#[doc(alias = "xdp_statistics")]
#[derive(Debug, Default, Copy, Clone)]
pub struct XdpStatistics {
    pub rx_dropped: u64,
    pub rx_invalid_descs: u64,
    pub tx_invalid_descs: u64,
}

#[repr(C)]
#[doc(alias = "xdp_statistics")]
#[derive(Debug, Default, Copy, Clone)]
#[non_exhaustive]
pub struct XdpStatisticsV2 {
    pub rx_dropped: u64,
    pub rx_invalid_descs: u64,
    pub tx_invalid_descs: u64,
    // Only set on >= Linux 5.9
    pub rx_ring_full: u64,
    // Only set on >= Linux 5.9
    pub rx_fill_ring_empty_descs: u64,
    // Only set on >= Linux 5.9
    pub tx_ring_empty_descs: u64,
}

impl Default for SockAddrXdp {
    fn default() -> Self {
        SockAddrXdp {
            family: libc::AF_XDP as u16,
            flags: 0,
            ifindex: 0,
            queue_id: 0,
            shared_umem_fd: 0,
        }
    }
}
