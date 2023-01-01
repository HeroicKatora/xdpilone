use core::cell::UnsafeCell;
use core::{num::NonZeroU32, ptr::NonNull};
use xdp_ral::xsk::{IfInfo, XskSocket, XskUmem, XskUmemConfig, XskSocketConfig};

// We can use _any_ data mapping.
#[repr(align(4096))]
struct PacketMap(UnsafeCell<[u8; 1 << 20]>);
// Safety: no direct data access, actually.
unsafe impl Sync for PacketMap {}

static MEM: PacketMap = PacketMap(UnsafeCell::new([0; 1 << 20]));

fn main() {
    let args = <Args as clap::Parser>::parse();

    // Register the packet buffer with the kernel, getting an XDP socket file descriptor for it.
    let mem = NonNull::new(MEM.0.get() as *mut [u8]).unwrap();
    let umem = unsafe { XskUmem::new(XskUmemConfig::default(), mem) }.unwrap();
    let info = ifinfo(&args).unwrap();

    // Let's use that same file descriptor for our packet buffer operations on the specified
    // network interface (queue 0).
    let sock = XskSocket::with_shared(&info, &umem).unwrap();
    // Get the fill/completion queue.
    let device = umem.fq_cq(&sock).unwrap();

    // The receive/transmit queue.
    let rxtx = umem.rx_tx(&sock, &XskSocketConfig {
        rx_size: NonZeroU32::new(16),
        tx_size: None,
        lib_flags: 0,
        xdp_flags: 0,
        bind_flags: 0,
    }).unwrap();

    assert!(rxtx.map_tx().is_err(), "did not provide a tx_size");
    let rx = rxtx.map_rx().unwrap();

    // Ready to bind, i.e. kernel to start doing things on the ring.
    umem.bind(&rxtx).unwrap();

    eprintln!("Success!");

    for _ in 0..(1 << 8) {
    }
}

#[derive(clap::Parser)]
struct Args {
    ifname: String,
}

fn ifinfo(args: &Args) -> Result<IfInfo, xdp_ral::Errno> {
    let mut bytes = String::from(&args.ifname);
    bytes.push('\0');
    let bytes = bytes.as_bytes();
    let name = core::ffi::CStr::from_bytes_with_nul(bytes).unwrap();
    let mut info = IfInfo::invalid();
    info.from_name(name)?;
    Ok::<IfInfo, _>(info)
}
