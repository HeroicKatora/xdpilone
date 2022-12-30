/// Rx/Tx descriptor.
///
/// The layout of this struct is part of the kernel interface.
#[repr(C)]
pub struct XdpDesc {
    pub addr: u64,
    pub len: u32,
    pub options: u32,
}
