# Solar

Solar serves 2 purposes:

Solar is a high-performance and memory-safe programming language that is like Rust with GC, where every reference can last as long as it likes. Types are algebraic (`struct` for product types and `enum` for sum types) and fields are stored inline.

Solar is also a target for any derived languages (similar to how Java's JVM is a target for Kotlin), with flexible memory management and semantics. Unlike Java's classes, structs can have fields of other structs that do not require indirection, and references can refer to either the whole instance of a struct or any inner field.

Solar's syntax is similar to Rust (with a few differences); see [`example.solar`](examples/example.solar) for an overview.

## Performance

Currently, programs that do not require heap allocations (such as [`loop.solar`](examples/loop.solar)) have the same performance as the equivalent C program.

LLVM is able to elide allocations (heap to stack, and stack to register), and programs that test this ([`loop2*.solar`](examples/)) have the same performance as the allocation-free base implementation.

## Sum types

Implementing sum types using tagged unions cannot support references to the contents of variants, since changing the variant can make memory contents invalid for the previous variant, while a reference still exists.

Solar solves this by using *tagged structs*:

```
enum Test {
  A(Int),
  B(fn()),
}
```

is encoded in memory as

```rs
struct Test {
  discriminant: u8,
  a: MaybeUninit<Int>,
  b: MaybeUninit<fn()>,
}
```

This is memory-safe, since a reference to a variant can be obtained only if that variant is currently used (and the variant's memory space is initialized), and changing the variant by assignment will change the discriminant and write to the new variant's memory space. The old variant's memory space is kept as-is, so references remain valid.

## Memory management

Solar is memory safe, even with values implemented with wide pointers (currently, references to unsized values and functions are 16 bytes). [Reading from these values cannot tear](https://www.ralfj.de/blog/2025/07/24/memory-safety.html), since assignment uses 128-bit atomics.

### Ideal implementation

Solar supports a concurrent moving GC with *partial frees*: In most languages, an allocation is a single block of memory with an address, and can be freed later. The GC expands this by reclaiming specific portions of an allocation when only parts of the allocation are reachable. For example, if I allocate a large array and only keep a reference to a single element, the rest of the array can be reclaimed.

However, doing this requires LLVM (or another codegen backend) to emit Solar stackmaps, which is currently not implemented.

### Current implementation

Currently, Solar uses a multithreaded semi-conservative stop-the-world GC. During each cycle, each thread's stack and registers, and small allocations (below a threshold) are scanned conservatively, and large allocations are scanned precisely using the allocation's metadata.

On program startup, a 26TB arena is allocated and divided into buckets, one per power-of-2 allocation size up to 1GB, with each bucket holding 1TB in total.

Conservative scanning becomes a fast O(1) lookup, since any word that looks like a potential memory address is scanned only if it belongs in the arena, and the allocation size can be found from the bucket index.
