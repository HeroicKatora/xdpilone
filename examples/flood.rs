//! This example demonstrates _flooding_ a network with packets.
//!
//! This will very aggressively poll the queues. Seriously, wasting time is an understatement. Do
//! not use in production and aim at a network interface with care!
use core::{mem::MaybeUninit, num::NonZeroU32, ptr::NonNull};
use xdpilone::xdp::XdpDesc;
use xdpilone::{BufIdx, IfInfo, Socket, SocketConfig, Umem, UmemConfig};

// We can use _any_ data mapping, so let's use a static one setup by the linker/loader.
#[repr(align(4096))]
struct PacketMap(MaybeUninit<[u8; 1 << 20]>);

fn main() {
    let args = <Args as clap::Parser>::parse();

    let alloc = Box::new(PacketMap(MaybeUninit::uninit()));
    // Register the packet buffer with the kernel, getting an XDP socket file descriptor for it.
    let mem = NonNull::new(Box::leak(alloc).0.as_mut_ptr()).unwrap();

    // Safety: we guarantee this mapping is aligned, and will be alive. It is static, after-all.
    let umem = unsafe { Umem::new(UmemConfig::default(), mem) }.unwrap();
    let info = ifinfo(&args).unwrap();

    // Let's use that same file descriptor for our packet buffer operations on the specified
    // network interface. Umem + Fill/Complete + Rx/Tx will live on the same FD.
    let sock = Socket::with_shared(&info, &umem).unwrap();
    // Get the fill/completion device (which handles the 'device queue').
    let device = umem.fq_cq(&sock).unwrap();

    // Configure our receive/transmit queues.
    let rxtx = umem
        .rx_tx(
            &sock,
            &SocketConfig {
                rx_size: None,
                tx_size: NonZeroU32::new(1 << 14),
                bind_flags: SocketConfig::XDP_BIND_NEED_WAKEUP,
            },
        )
        .unwrap();

    assert!(rxtx.map_rx().is_err(), "did not provide a rx_size");
    // Map the TX queue into our memory space.
    let tx = rxtx.map_tx().unwrap();

    // Ready to bind, i.e. kernel to start doing things on the ring.
    umem.bind(&rxtx).unwrap();

    // Setup one frame we're going to use, repeatedly.
    // We only need its descriptor for the TX queue.
    let desc = {
        let mut frame = umem.frame(BufIdx(1)).unwrap();
        // Safety: we are the unique thread accessing this at the moment.
        prepare_buffer(frame.offset, unsafe { frame.addr.as_mut() }, &args)
    };

    eprintln!("Connection up!");

    // Bring our bindings into an 'active duty' state.
    let mut tx = tx;
    let mut device = device;

    let start = std::time::Instant::now();

    let batch: u32 = args.batch.unwrap_or(1 << 10);
    let total: u32 = args.total.unwrap_or(1 << 20);

    let mut sent = 0;
    let mut completed = 0;

    // some nice stats to track and later report.
    let mut stat_loops = 0;
    let mut stat_stall = 0;
    let mut stat_woken = 0;
    let mut tx_log_batch = [0; 33];
    let mut cq_log_batch = [0; 33];

    eprintln!(
        "Dumping {} B with {} packets!",
        total as f32 * desc.len as f32,
        total
    );

    eprintln!("The description is {:?}", desc,);

    while !(sent == completed && sent == total) {
        let sent_now: u32; // Number of buffers enqueued in this iteration.
        let comp_now: u32; // Number of completions reaped in this iteration.

        {
            let send_batch = total.saturating_sub(sent).min(batch);
            // Try to add descriptors to the transmit buffer.
            let mut writer = tx.transmit(send_batch);
            let bufs = core::iter::repeat(desc);
            sent_now = writer.insert(bufs);
            writer.commit();
        }

        // It may be necessary to wake up. This is costly, in relative terms, so we avoid doing
        // it when the kernel proceeds without us. We detect this by checking if both queues
        // failed to make progress for some time. And then only do it once.
        if tx.needs_wakeup() {
            tx.wake();
            stat_woken += 1;
        }

        {
            let comp_batch = sent.saturating_sub(completed).min(batch);
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
            stat_stall += 1;
        }

        // Stat tracking..
        sent += sent_now;
        completed += comp_now;
        stat_loops += 1;

        tx_log_batch[32 - sent_now.leading_zeros() as usize] += 1;
        cq_log_batch[32 - comp_now.leading_zeros() as usize] += 1;
    }

    // Dump all measurements we took.
    let end = std::time::Instant::now();
    let secs = end.saturating_duration_since(start).as_secs_f32();
    let packets = completed as f32;
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
        stat_loops, stat_stall, stat_woken
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
