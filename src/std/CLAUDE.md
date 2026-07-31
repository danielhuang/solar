# Standard library

`lib.solar` is the `@std` entry point. Public modules must be re-exported from
there. Imports are resolved relative to `src/std`, including subdirectories.

## API rules

- Add `///` documentation to every public declaration.
- Keep low-level operations in `@intrinsics`; expose user-facing wrappers from
  `@std`.
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

File APIs have interpreter implementations in `src/interp_io.rs`. Threads,
futexes, and sockets are native-runtime only; tests for them belong in the
compiled-only suite.

When adding a fallible intrinsic, keep its exception text identical in the AST
interpreter, IR interpreter, and native runtime.
