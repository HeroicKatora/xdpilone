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
                tx_size: NonZeroU32::new(1 << 12),
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

    eprintln!("Connection up!");

    let desc = XdpDesc {
        addr: frame.offset,
        len: ARP.len() as u32,
        options: 0,
    };

    let mut tx = tx;
    let mut device = device;
    tx.wake();

    let start = std::time::Instant::now();

    const BATCH: u32 = 1 << 10;
    const TOTAL: u32 = 1 << 20;
    const WAKE_THRESHOLD: i32 = 1 << 4;

    let mut sent = 0;
    let mut completed = 0;
    let mut stall_count = WAKE_THRESHOLD;

    let mut stat_loops = 0;
    let mut stat_woken = 0;
    let mut rx_log_batch = [0; 33];
    let mut tx_log_batch = [0; 33];

    loop {
        stat_loops += 1;

        if sent == completed && sent == TOTAL {
            break;
        }

        let send_batch = TOTAL.saturating_sub(sent).min(BATCH);
        let comp_batch = sent.saturating_sub(completed).min(BATCH);

        let sent_now;
        let comp_now;

        {
            // Try to add values to the transmit buffer.
            let mut writer = tx.transmit(send_batch);
            let bufs = core::iter::repeat(desc);
            sent_now = writer.insert(bufs);
            writer.commit();
        }

        {
            // Try to dequeue some completions.
            let mut reader = device.complete(comp_batch);
            let mut comp_temp = 0;

            while reader.read().is_some() {
                comp_temp += 1;
            }

            comp_now = comp_temp;
            reader.release();
        }

        if sent_now == 0 && comp_now == 0 {
            stall_count += 1;
        } else {
            stall_count = 0;
        }

        sent += sent_now;
        completed += comp_now;

        rx_log_batch[sent_now.leading_zeros() as usize] += 1;
        tx_log_batch[comp_now.leading_zeros() as usize] += 1;

        if stall_count > WAKE_THRESHOLD {
            // It may be necessary to wake up. This is costly, in relative terms, so we avoid doing
            // it when the kernel proceeds without us. We detect this by checking if both queues
            // failed to make progress for some time.
            tx.wake();
            stat_woken += 1;
            stall_count = 0;
        }
    }

    let end = std::time::Instant::now();
    let secs = end.saturating_duration_since(start).as_secs_f32();
    let packets = completed as f32;
    let bytes = packets * ARP.len() as f32;

    eprintln!(
        "{:?} s; {} pkt; {} pkt/s; {} B/s",
        secs,
        packets,
        packets / secs,
        bytes / secs
    );

    eprintln!("Statistics\nLoops: {}; Woken: {}", stat_loops, stat_woken);

    eprintln!("Rx Batch size (log): {:?}", rx_log_batch);
    eprintln!("Tx Batch size (log): {:?}", rx_log_batch);
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
