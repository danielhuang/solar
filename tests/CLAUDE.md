# Tests

Test groups have distinct backend coverage:

- `tests/runtime`: AST interpreter, IR interpreter, and debug native codegen;
  outputs must match.
- `tests/typecheck`: compiler diagnostics.
- `tests/parser`: CST-to-AST behavior and source spans.
- `tests/multi_file`: imports, visibility, provenance, and cross-file symbols.
- `tests/compile_only`: native-only features such as threads and sockets.
- Dedicated release integration tests exercise the collector and LLVM passes.

Use `test-utils` helpers instead of duplicating pipeline setup. Debug native
tests use ASAN and exercise the unoptimized write-barrier/collector path.
Release integration tests additionally exercise LTO and allocation
specialization.

GC-San tests cover both optimized and unoptimized `CompileOptions`: both run the
collector with arena access checks and monotonic allocation frontiers.

Tests that perform file or directory I/O beyond standard-stream writes belong
in `compile_only` because those APIs use the native-only `syscall` intrinsic.
Standard-stream coverage remains in `runtime` so both interpreters continue to
exercise `file_std*` and `file_write_partial`.

Keep runtime exception messages identical across all backends. Add regression
fixtures near the subsystem they exercise and avoid temporary probe tests or
machine-specific paths.

Run the full suite with:

```bash
cargo test --workspace
```
