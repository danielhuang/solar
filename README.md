# Solar

Solar serves 2 purposes:

Solar is a high-performance and memory-safe programming language that is like Rust with GC, where every reference can last as long as it likes. Types are algebraic (`struct` for product types and `enum` for sum types) and fields are stored inline.

Solar is also a target for any derived languages (similar to how Java's JVM is a target for Kotlin), with flexible memory management and semantics. Unlike Java's classes, structs can have fields of other structs that do not require indirection, and references can refer to either the whole instance of a struct or any inner field.

Solar's syntax is similar to Rust (with a few differences); see [`example.solar`](examples/example.solar) for an overview.

## Performance

Solar lowers to native code and gets optimal performance for workloads that are not allocation-heavy, such as [`sieve.solar`](examples/sieve.solar).

For allocation-heavy workloads, Solar's GC is faster than Java's (including Shenandoah, ZGC, G1, and Parallel), .NET's, and Go's GCs. Additionally, Solar's allocator is faster than glibc's allocator (~2x).

See [`bench/README.md`](bench/README.md) for details.

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

## Extension methods

`method(ext)` declares a method without requiring any parameter type to be
defined in the same file:

```rs
pub method(ext) doubled(self: Int) -> Int { self * 2 }
```

An extension is available in its defining file and in files that import its
name, directly or through a wildcard or public re-export. Importing only its
module does not bring the extension into scope. Generic bodies use the scope
where they were defined. Normal methods retain their existing global lookup.

Applicable extensions must not conflict with each other or with a normal
method; a more specific signature does not override another applicable method.
Identical signatures in disjoint extension scopes are allowed and use distinct,
module-qualified symbols. `method(ext, inline)` also requests inlining.

## Copies and moves

A copy constructor takes a reference to its original value and returns a new value:

```rs
struct Value { number: Int }

fn Value::(original: &Value) -> Value {
  Value { number: original@.number }
}
```

Call it explicitly with `Value(old&)`. By-value uses call it implicitly when the
original value is still needed:

```rs
let old = Value { number: 1 };
let copied = old; // Calls Value(old&), since old is used again.
let moved = old;  // Transfers the final use without calling the constructor.
```

The compiler moves a value only when it can prove that no later use or surviving
alias needs the original. References that may escape, including references kept
by a copy constructor, prevent such a move. This applies to arguments, returns,
and aggregate fields as well as variable assignments.

Without a custom constructor, copying a struct, enum, or array recursively copies
its fields or elements. Their copy constructors run when present. Shared `&T`
references keep sharing their target; owned `^T` values copy their pointees only
when a copy is required. A last-use move transfers the owned pointer instead.

## Memory management

Solar is memory safe, even with values implemented with wide pointers (currently, references to unsized values and functions are 16 bytes). [Reading from these values cannot tear](https://www.ralfj.de/blog/2025/07/24/memory-safety.html), since assignment uses 128-bit atomics.

Solar uses a multithreaded semi-conservative concurrent GC. During each cycle, each thread's stack and registers, and small allocations (below a threshold) are scanned conservatively, and large allocations are scanned precisely using the allocation's metadata.

On program startup, a 26TB arena is allocated and divided into buckets, one per power-of-2 allocation size up to 1GB, with each bucket holding 1TB in total.

Conservative scanning becomes a fast O(1) lookup, since any word that looks like a potential memory address is scanned only if it belongs in the arena, and the allocation size can be found from the bucket index.

