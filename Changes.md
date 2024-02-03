## v1.0.3

- Hide an unimplemented function sketch which was accidentally left over from
  previous experiments. Calling it always panics. The method will remain
  accessible for compatibility reasons (SemVer).

## v1.0.2

- Implement `Iterator` for `ReadRx` and `ReadComplete`.
- Document queue interaction adapters with intended workflow.
