# Standard library

`lib.solar` is the `@std` entry point. Public modules must be re-exported from
there. Imports are resolved relative to `src/std`, including subdirectories.

## API rules

- Add `///` documentation to every public declaration.
- Keep low-level operations in `@intrinsics`; expose user-facing wrappers from
  `@std`.
- `size_of#[T]()` returns the packed byte size of a statically sized type.
- `ref_eq(a, b)` compares reference identity.
- The blanket `operator_eq(self: &&T, other: &&T)` compares referenced values.
- Reflective equality and hashing for structs and enums require public fields.
- HashMap keys must be sized.

Arrays are value types. Dereferencing an array reference copies the array, so
mutating code should keep the reference and index through it:

```solar
let slots = self@.slots;
slots@[i] = value;
```

## Backend support

Standard-stream and partial-write file intrinsics have interpreter
implementations in `src/interp_io.rs`. Other file, directory, and socket
operations use the native-only `syscall` intrinsic, so their tests belong in
the compiled-only suite. Threads and futexes are native-runtime only too.

When adding a fallible intrinsic, keep its exception text identical in the AST
interpreter, IR interpreter, and native runtime.
