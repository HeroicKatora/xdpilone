## v1.2.0

- Introduced `XdpStatisticsV2`, a forward compatible struct for fetching
  statistics related to an XDP socket. The kernel in fills the passed struct
  depending on the indicated size, older kernels will leave some of its members
  untouched. This struct is marked non-exhaustive so that any future fields
  added by the kernel can be supported in a minor change without requiring
  further separate types.

## v1.1.1

- The flags passed via `SocketFlags::bind_flags` are now applied to all bind
  calls where previously they would only apply to sockets sharing the umem with
  another prior socket.

## v1.1

- Added `DeviceQueue::bind` for binding queues from multiple different
  interfaces to the same underlying `umem`. Previously only a single socket for
  each additional queue could be bound when the same socket set up both
  fill/completion rings as well as receive/transmit rings.
- Note: I'm currently not entirely comfortable with the types of the `bind`
  argument. They are not generic enough to cover all possible usages—the socket
  of fq/cq socket itself is sufficient but a `User` with rx/tx sockopts is
  required. At the same time however the types barely guard invariants that
  would detect some misuse or failure paths at compile time. Also to-be-used
  bind flags are associated with the socket as a `User` struct not as an
  independent argument to the `bind` call.
- Note: Please open PRs resolving this either way, not issues.
- Rename `Errno::new` to `Errno::last_os_error` aligning it with the standard
  library for this construct. The old name is kept as a documentation hidden
  method for compatibility.

## v1.0.5

- Discovered that the `XdpUmemReg` contains padding, being passed to the kernel
  as the `tx_metadata_len` option. This would should up as spurious invalid
  argument (EINVAL) errors from the interpretation of the field.

## v1.0.4

- No code changes.
- Clarified status as feature-complete, passively maintained.
- Updated some documentation.


## v1.0.3

- Hide an unimplemented function sketch which was accidentally left over from
  previous experiments. Calling it always panics. The method will remain
  accessible for compatibility reasons (SemVer).

## v1.0.2

- Implement `Iterator` for `ReadRx` and `ReadComplete`.
- Document queue interaction adapters with intended workflow.
