//! This example demonstrates _flooding_ a network with packets.
//!
//! Aim at a network interface with care!
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};
use core::{num::NonZeroU32, ptr::NonNull};
use xdpilone::xdp::XdpDesc;
use xdpilone::xsk::{
    BufIdx, IfInfo, XskSocket, XskSocketConfig, XskTxRing, XskUmem, XskUmemConfig,
};

// We can use _any_ data mapping, so let's use a static one setup by the linker/loader.
#[repr(align(4096))]
struct PacketMap(UnsafeCell<[u8; 1 << 20]>);
// Safety: no instance used for unsynchronized data access.
unsafe impl Sync for PacketMap {}

static MEM: PacketMap = PacketMap(UnsafeCell::new([0; 1 << 20]));

fn main() {
    let args = <Args as clap::Parser>::parse();

    // Register the packet buffer with the kernel, getting an XDP socket file descriptor for it.
    let mem = NonNull::new(MEM.0.get() as *mut [u8]).unwrap();

    // Safety: we guarantee this mapping is aligned, and will be alive. It is static, after-all.
    let umem = unsafe { XskUmem::new(XskUmemConfig::default(), mem) }.unwrap();
    let info = ifinfo(&args).unwrap();

    // Let's use that same file descriptor for our packet buffer operations on the specified
    // network interface. Umem + Fill/Complete + Rx/Tx will live on the same FD.
    let sock = XskSocket::with_shared(&info, &umem).unwrap();
    // Get the fill/completion device (which handles the 'device queue').
    let device = umem.fq_cq(&sock).unwrap();

    let rxtx_config = XskSocketConfig {
        rx_size: None,
        tx_size: NonZeroU32::new(1 << 12),
        lib_flags: 0,
        xdp_flags: 0,
        bind_flags: 0,
    };

    let mut tx_queues = vec![];

    let mut sock = Some(XskSocket::with_shared(&info, &umem).unwrap());
    for _ in 0..(args.threads.map_or(1, NonZeroU32::get)) {
        let sock = sock.take().unwrap_or_else(|| XskSocket::new(&info).unwrap());

        // Configure our receive/transmit queues.
        let rxtx = umem.rx_tx(&sock, &rxtx_config).unwrap();
        assert!(rxtx.map_rx().is_err(), "did not provide a rx_size");
        // Map the TX queue into our memory space.
        let tx = rxtx.map_tx().unwrap();

        // Ready to bind, i.e. kernel to start doing things on the ring.
        umem.bind(&rxtx).unwrap();
        tx_queues.push(tx);
    }

    // Setup one frame we're going to use, repeatedly.
    // We only need its descriptor for the TX queue.
    let desc = {
        let mut frame = umem.frame(BufIdx(0)).unwrap();
        // Safety: we are the unique thread accessing this at the moment.
        prepare_buffer(frame.offset, unsafe { frame.addr.as_mut() }, &args)
    };

    eprintln!("Connection up!");

    // Bring our bindings into an 'active duty' state.
    let mut device = device;

    let start = std::time::Instant::now();

    let batch: u32 = args.batch.unwrap_or(1 << 10);
    let total: u32 = args.total.unwrap_or(1 << 20);
    const WAKE_THRESHOLD: u32 = 1 << 4;

    let sent_reserved = AtomicU32::new(0);
    let sent = AtomicU32::new(0);
    let completed = AtomicU32::new(0);
    let stall_count = AtomicU32::new(0);

    let mut stat_loops = 0;
    let mut stat_stall = 0;
    let stat_woken = AtomicU32::new(0);
    let tx_log_batch = [0; 33].map(AtomicU32::new);
    let mut cq_log_batch = [0; 33];

    eprintln!(
        "Dumping {} B with {} packets!",
        total as f32 * desc.len as f32,
        total
    );

    let completer = || loop {
        let current = completed.load(Ordering::Relaxed);

        if current == total {
            break;
        }

        // Number of completions reaped in this iteration.
        let comp_now: u32;
        let comp_batch = sent
            .load(Ordering::Acquire)
            .saturating_sub(current)
            .min(batch);

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

        if comp_now == 0 {
            stall_count.fetch_add(1, Ordering::Relaxed);
            stat_stall += 1;
        }

        completed.fetch_add(comp_now, Ordering::Release);
        stat_loops += 1;

        cq_log_batch[32 - comp_now.leading_zeros() as usize] += 1;
    };

    let sender = |mut tx: XskTxRing| {
        let mut stall_threshold = WAKE_THRESHOLD;
        loop {
            if sent.load(Ordering::Relaxed) >= total && completed.load(Ordering::Relaxed) >= total {
                break;
            }

            let send_batch = loop {
                // Reserve some of these buffers. Relaxed loads because we don't synchronize with any
                // other memory locations, only atomicity.
                match sent_reserved.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    Some(n + total.saturating_sub(n).min(batch))
                }) {
                    // Break with the number of updated total.
                    Ok(prev) => break total.saturating_sub(prev).min(batch),
                    Err(_) => continue,
                }
            };

            let sent_now: u32;

            {
                // Try to add descriptors to the transmit buffer.
                let mut writer = tx.transmit(send_batch);
                let bufs = core::iter::repeat(desc);
                sent_now = writer.insert(bufs);
                writer.commit();
            }

            if stall_count.load(Ordering::Relaxed) > stall_threshold {
                // It may be necessary to wake up. This is costly, in relative terms, so we avoid doing
                // it when the kernel proceeds without us. We detect this by checking if both queues
                // failed to make progress for some time.
                tx.wake();
                stat_woken.fetch_add(1, Ordering::Relaxed);
                stall_threshold += WAKE_THRESHOLD;
            }

            // Stat tracking..
            sent_reserved.fetch_sub(send_batch - sent_now, Ordering::Relaxed);
            sent.fetch_add(sent_now, Ordering::Release);

            tx_log_batch[32 - sent_now.leading_zeros() as usize].fetch_add(1, Ordering::Relaxed);
        }
    };

    std::thread::scope(|scope| {
        scope.spawn(completer);

        for tx in tx_queues {
            scope.spawn(|| sender(tx));
        }
    });

    // Dump all measurements we took.
    let end = std::time::Instant::now();
    let secs = end.saturating_duration_since(start).as_secs_f32();

    let packets = completed.into_inner() as f32;
    let bytes = packets * desc.len as f32;

    eprintln!(
        "{:?} s; {} pkt; {} pkt/s; {} B/s; {} L1-B/s",
        secs,
        packets,
        packets / secs,
        bytes / secs,
        // Each frame has 7(Preamble)+1(delimiter)+12(IGP) Ethernet overhead.
        (bytes + packets * 20.) / secs,
    );

    eprintln!(
        "Statistics\nLoops: {}; stalled: {}; wake/sys-call: {}",
        stat_loops,
        stat_stall,
        stat_woken.into_inner()
    );

    eprintln!("Tx Batch size (log2): {:?}", tx_log_batch);
    eprintln!("Cq Batch size (log2): {:?}", cq_log_batch);
}

fn prepare_buffer(offset: u64, buffer: &mut [u8], args: &Args) -> XdpDesc {
    buffer[..ARP.len()].copy_from_slice(&ARP[..]);
    let extra = args.length.unwrap_or(0).saturating_sub(ARP.len() as u32);

    XdpDesc {
        addr: offset,
        len: ARP.len() as u32 + extra,
        options: 0,
    }
}

#[derive(clap::Parser)]
struct Args {
    /// The name of the interface to use.
    ifname: String,
    /// Overwrite the queue_id.
    #[arg(long = "queue-id")]
    queue_id: Option<u32>,
    /// Maximum number of queue operations in a single loop.
    #[arg(long = "batch-size")]
    batch: Option<u32>,
    /// The total number of packets to enqueue on the NIC.
    #[arg(long = "packet-total")]
    total: Option<u32>,
    /// The count of bytes in each test packet to flood.
    #[arg(long = "packet-length")]
    length: Option<u32>,
    #[arg(long = "threads")]
    threads: Option<NonZeroU32>,
}

fn ifinfo(args: &Args) -> Result<IfInfo, xdpilone::Errno> {
    let mut bytes = String::from(&args.ifname);
    bytes.push('\0');
    let bytes = bytes.as_bytes();
    let name = core::ffi::CStr::from_bytes_with_nul(bytes).unwrap();

    let mut info = IfInfo::invalid();
    info.from_name(name)?;
    if let Some(q) = args.queue_id {
        info.set_queue(q);
    }

    Ok(info)
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
