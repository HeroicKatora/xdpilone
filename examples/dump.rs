use core::cell::UnsafeCell;
use core::{num::NonZeroU32, ptr::NonNull};
use xdp_ral::xdp::XdpDesc;
use xdp_ral::xsk::{BufIdx, IfInfo, XskSocket, XskSocketConfig, XskUmem, XskUmemConfig};

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
    let rxtx = umem
        .rx_tx(
            &sock,
            &XskSocketConfig {
                rx_size: None,
                tx_size: NonZeroU32::new(16),
                lib_flags: 0,
                xdp_flags: 0,
                bind_flags: 0,
            },
        )
        .unwrap();

    assert!(rxtx.map_rx().is_err(), "did not provide a rx_size");
    let tx = rxtx.map_tx().unwrap();
    // Ready to bind, i.e. kernel to start doing things on the ring.
    umem.bind(&rxtx).unwrap();

    // The frame we're going to use.
    let mut frame = umem.frame(BufIdx(0)).unwrap();

    {
        let buffer: &mut [u8] = unsafe { frame.addr.as_mut() };
        buffer[..ARP.len()].copy_from_slice(&ARP[..])
    }

    eprintln!("Success!");

    let desc = XdpDesc {
        addr: frame.offset,
        len: ARP.len() as u32,
        options: 0,
    };

    let mut tx = tx;
    let mut device = device;
    for _ in 0..(1 << 8) {
        while {
            let mut writer = tx.transmit(1);
            writer.insert(core::iter::once(desc)) == 0 || {
                writer.commit();
                false
            }
        } {
            core::hint::spin_loop()
        }

        eprintln!("Transmitting!");
        tx.wake();

        while {
            let mut reader = device.complete(1);
            reader.read().is_none() || {
                reader.release();
                false
            }
        } {
            core::hint::spin_loop()
        }

        eprintln!("Sent!");
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

#[rustfmt::skip]
static ARP: [u8; 14+28] = [
    0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
    0x31, 0x32, 0x33, 0x34, 0x35, 0x36,
    0x08, 0x06,

    0x00, 0x01,
    0x08, 0x00, 0x06, 0x04,
    0x00, 0x01,
    0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
    0x21, 0x22, 0x23, 0x24,
    0x31, 0x32, 0x33, 0x34, 0x35, 0x36,
    0x41, 0x42, 0x43, 0x44,
];
