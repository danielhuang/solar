# Tests

- Use `test-utils` helpers instead of duplicating pipeline setup.
- Keep generated sources and native binaries in a temporary directory owned by
  the test, and retain its guard until execution and assertions finish.
- Use explicit collection requests in GC lifetime tests; avoid allocation
  pressure as a collection trigger.
- Synchronize finalizer tests explicitly instead of relying on timing.
- Exercise both optimized and unoptimized builds in GC-San tests.
- Keep standard-stream tests in the shared runtime suite so all backends are
  exercised.
- Add regression fixtures near the subsystem they exercise. Avoid temporary
  probe tests and machine-specific paths.
