# Standard library

`lib.solar` is the `@std` entry point. Public modules must be re-exported from
there. Imports are resolved relative to `src/std`, including subdirectories.

## API rules

- Add `///` documentation to every public declaration.
- Keep low-level operations in `@intrinsics`; expose user-facing wrappers from
  `@std`.
- `size_of#[T]()` returns the laid-out byte size of a statically sized type,
  including C-compatible padding for `struct(repr(C))`.
- `mem::transmute#[T, out U](T) -> U` is safe only when the compiler can prove
  that the equal-sized source has no padding or uninitialized regions and the
  destination accepts every bit pattern. `mem::transmute_unchecked` bypasses
  those validity checks but remains size-checked and requires an explicit unsafe
  block.
- `mem::transmute_ref(&T, Uint) -> &U` reuses the source address and supplies
  explicit metadata for an unsized destination. The caller owns all validity,
  alignment, and allocation-boundary obligations.
- `mem::offset_ref(&T, Int) -> &T` performs unsafe signed element-wise reference
  arithmetic and requires `T` to be sized.
- `black_box(T)` returns its input after passing a non-escaping reference to
  Rust's optimizer barrier.
- `gc_keepalive(&T)` keeps its argument reachable through the call without
  retaining it.
- `Any(&T)` erases a sized referent's concrete type while preserving reference
  aliasing; `Any.downcast#[T]()` returns `&?T` without exposing the private tag.
- `ref_eq(a, b)` compares reference identity.
- The blanket `operator_eq(self: &&T, other: &&T)` compares referenced values.
- Reflective equality and hashing for structs and enums require public fields.
- HashMap keys must be sized.
- Constructor-like APIs are empty-name associated functions (`fn Type::(...)`)
  and are invoked as `Type(...)`.

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
the compiled-only suite. Raw syscall failures must be formatted through
`linux::check_syscall` so exceptions include both the registered Linux error
message and errno. Threads and futexes are native-runtime only too.

When adding a fallible intrinsic, keep its exception text identical in the AST
interpreter, IR interpreter, and native runtime.
