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
tests use ASAN. Collector behavior must be tested with release codegen because
debug codegen disables collection.

Keep runtime exception messages identical across all backends. Add regression
fixtures near the subsystem they exercise and avoid temporary probe tests or
machine-specific paths.

Run the full suite with:

```bash
cargo test --workspace
```
