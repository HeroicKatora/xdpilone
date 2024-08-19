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
