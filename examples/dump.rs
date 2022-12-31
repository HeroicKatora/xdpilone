use core::cell::UnsafeCell;
use core::{mem::MaybeUninit, ptr::NonNull};
use xdp_ral::xsk::{IfInfo, XskSocket, XskUmem, XskUmemConfig, XskSocketConfig};

// We can use _any_ data mapping.
#[repr(align(4096))]
struct PacketMap(UnsafeCell<[u8; 1 << 20]>);
// Safety: no direct data access, actually.
unsafe impl Sync for PacketMap {}

static MEM: PacketMap = PacketMap(UnsafeCell::new([0; 1 << 20]));

fn main() {
    let args = <Args as clap::Parser>::parse();

    let mem = NonNull::new(MEM.0.get() as *mut [u8]).unwrap();
    let mut umem = unsafe { XskUmem::new(XskUmemConfig::default(), mem) }.unwrap();

    let info = {
        let bytes = b"enp8s0\0";
        let name = core::ffi::CStr::from_bytes_with_nul(bytes).unwrap();
        let mut info = IfInfo::invalid();
        info.from_name(name).unwrap();
        info
    };

    let sock = XskSocket::new(&info).unwrap();
    let device = umem.fq_cq(&sock).unwrap();

    umem.bind(&sock, &XskSocketConfig {
        rx_size: 2048,
        tx_size: 2048,
        flags: 0,
        xdp_flags: 0,
    }).unwrap();

    eprintln!("Success!");
}

#[derive(clap::Parser)]
struct Args {}
