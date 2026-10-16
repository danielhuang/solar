# Writing Solar programs

Solar has Rust-like syntax, mutable references, value copying, and garbage
collection. This guide is for writing `.solar` programs. Behavior is specified
only where an example-program test covers it; uncovered behavior is implementation
defined and subject to change. GC performance and codegen regression tests do
not establish user-visible guarantees. Do not infer Rust semantics from familiar
syntax.

## Run and check

Use `solar check main.solar`, `solar fmt main.solar helper.solar`,
`solar run main.solar --release`, or
`solar compile main.solar ./program --release`. In this repository, replace
`solar` with `cargo run --release --` if needed. The entry point is `fn main()`
with no parameters and a `()` return. Prefer release codegen for normal programs.

```solar
fn main() {
    let values = [10, 20, 30];
    let total = 0;
    for value in values { total = total + value@; }
    println(total); // 60
    println("hello"&);
}
```

## Syntax and types

- `//` comments; `///` documentation comments. Use `;` after bindings,
  assignments, calls, and returns; brace-delimited control flow needs no `;`.
  A block's final expression without `;` supplies its value; a final expression
  with `;` is discarded. Empty blocks return `()`.
- `let x = 1;` or `let x: Int = 1;`. Bindings and parameters are mutable without
  `mut`. Repeating `let x = ...;` shadows the previous binding.
- Integers: `Int8/16/32/64`, `Uint8/16/32/64`, and pointer-sized `Int`, `Uint`.
  Unsuffixed integer literals are `Int`, even with a different type annotation.
  Suffixes: `1u` (`Uint`), `1i8`, `1u8`, etc.; bases: `0b101`, `0o17`, `0xFF`.
  Write negative numbers as `0 - 5` (with matching operand types).
- Floats: `Float32`, `Float64`; literals require `f32`, `f64`, or `f` (`Float64`):
  `1f`, `2.5f32`. Convert explicitly: `Uint(x)`, `Int(x)`, `Float64(x)`.
- `Bool`: `true`, `false`. Conditions require `Bool`.
- Tuples: `(Int, Bool)`, `(1, true)`; fields `t.0`, `t.1`. Unit is `()`.
  Tuple structs: `struct Pair(Int, Bool);`, constructed with `Pair(1, true)`.
- Destructure bindings/parameters with `(a, b)`, `Point { x, y: renamed }`,
  or `[a, b]`; `_` discards a component. Array destructuring requires exact length.
- `const LIMIT: Int = 10;` holds a literal, at top level or inside a block.
  `static COUNT = 0;` is top-level mutable state with a literal initializer;
  `static(thread_local) COUNT = 0;` gives per-thread state. Initialize more
  complex global state in `main`, e.g. through an initially null reference.

## Values, references, and copying

Assignments, arguments, and returns copy values: non-reference struct fields
and array elements copy independently. There are no Rust-style moves or borrow
lifetimes. Use references to share state and avoid copying large values.

| Type / expression | Meaning |
| --- | --- |
| `&T` / `x&` | Mutable shared reference; copying aliases the same storage |
| `^T` / `x^` | Unique reference; copying deep-copies its pointee |
| `r@` | Dereference, also usable as an assignment target |
| `&?T` / `null#[T]` | Nullable reference / typed null |

Examples: `let r = 5&; r@ = 9;`, `p@.field`, `a[0u]&`.
References to locals and interior fields can escape their original scope.
Copying a value containing `&T` preserves shared references; copying `^T` still
shares any nested `&T` fields. `x^` creates independent storage; use `x&` to
refer to the existing place. Dereference explicitly for field access.

`&T` coerces to `&?T`, but not conversely. Check `r != null#[T]`; `r@` throws
on null, and `r@&` obtains a non-null reference with that check.
`ref_eq(a, b)` tests reference identity; ordinary `==` on shared references
compares pointee values.

## Arrays and byte strings

- `[T]` is an unsized, non-resizable array; `[T; N]` has a fixed integer length.
  Unsized values can be locals and the last field of a struct, but function
  parameters/returns must be sized: use `&[T]`, `^[T]`, or `[T; N]` there.
- Literals: `[1, 2]`; empty arrays require `[]#[Int]`, even with a binding
  annotation. `[1, 2]#[Int]` checks the element type.
- Repeat: `[0; 5u]`. Generate by index: `[5u; \i: Uint i * i]`.
- Array indices, slice bounds, and runtime lengths use `Uint`: `a[0u]`,
  `a[1u..3u]`, `a&.len()`. Bounds are checked; ranges exclude the end.
- `a[i..j]&` references the original elements; reading a slice into a new
  value copies it. For slice assignment, use equal-length source/destination
  ranges; the elements are copied.
  `a + b` concatenates arrays; `a == b` compares elements.
- `"text"` is `[Uint8]`; `"text"&` is `&[Uint8]`. Character literals such as
  `'A'`, `'\n'` are `Uint8` bytes. Use `"hello " + "world"` for concatenation,
  `("answer: " + n.to_string()@)&` for assembled text.

## Control flow and operators

- `if condition { ... } else if other { ... } else { ... }`;
  value-producing `if` requires `else`: `let n = if ok { 1 } else { 0 };`.
- `while condition { ... }`; `for i in 0..10 { ... }` (exclusive end, matching
  integer bound types); `loop { ... }`. `break`/`continue` target the innermost
  loop; only `loop` accepts a value: `let n = loop { break 42; };`.
- `for x in array { ... }` yields references (`x@` reads, `x@ = ...` writes).
  General iteration uses an `iter` method followed by `next` on the iterator
  reference, returning `Option#[T]`; the loop receives the yielded `T`.
- Arithmetic: `+ - * / %`; checked integer overflow and integer division by
  zero throw. Wrapping integer arithmetic: `++ -- **` (add, subtract, multiply).
  Comparisons: `== != < <= > >=`; logical `! && ||` (`&&`/`||` short-circuit).
  Bitwise: `& | ^ << >>`, `!` complements integers. Binary numeric operands
  require matching types, except shift counts may use another integer type.
  Use explicit assignment, e.g. `n = n + 1;`.

## Structs, enums, functions, and generics

```solar
struct Point { x: Int, y: Int }
enum Shape { Point, Circle(Int) } // one payload type per variant
struct Box#[T] { value: T }
type Bytes = &[Uint8];

fn add(a: Int, b: Int) -> Int { a + b }
fn identity#[T](x: T) -> T { x }
fn bytes_of#[T, out U](x: T) -> Uint { size_of#[U]() }
method set_x(self: &Point, x: Int) { self@.x = x; }
fn Point::new(x: Int, y: Int) -> Point { Point { x: x, y: y } }
fn Point::(x: Int, y: Int) -> Point { Point::new(x, y) }
fn#[T] Box#[T]::(value: T) -> Box#[T] { Box#[T] { value: value } }
```

- Construct named structs with every field: `Point { x: 1, y: 2 }`.
  Associated functions use `Point::new(1, 2)`; `fn Point::(...)` defines a
  constructor called as `Point(1, 2)`. A method is a separate `method`
  declaration, called as `p&.set_x(3)` for a reference receiver.
- Generic types use `#[...]`: `Box#[Int] { value: 7 }`, `Option#[Int]::Some(7)`.
  Function/method ordinary type parameters are inferred from arguments and
  cannot be supplied explicitly: `identity(7)`. Parameters used only for
  output must be `out`; they follow inferred parameters and must be explicit:
  `bytes_of#[Bool](7)` infers `T = Int`, supplies `U = Bool`.
- Return types may be inferred from the body; explicit `-> T` is useful for
  recursion. `return value;` exits early; `return;` returns unit.
  Overloads can share a name with different parameter types.
- Defaults define optional keyword parameters after required parameters:
  `fn ramp(start: Int, step = 1) -> Int { start + step }`; call
  `ramp(5, step = 2)`. Defaults are literals; pass these parameters by name.
- Functions are values: `let f: fn(Int, Int) -> Int = add;`.
  Closures: `\x: Int x + 1`, `\x: Int, y: Int { x + y }`, `\ { work(); }`.
  Parameter types can be inferred from a function-type context. Closures and
  nested `fn` declarations capture outer bindings by reference and can escape.
- Match enums: `match s { Shape::Point => 0, Shape::Circle(r) => r }`.
  Cover all variants or end with `_ => ...` / `other => ...`. Integer matches
  use literal cases plus a fallback: `match n { 0 => 1, other => other }`.
  Match payload bindings alias their source: assigning to `r` updates `s`.
- Operator methods, e.g. `method operator_add(self: &Point, rhs: &Point)`,
  implement `a + b` through borrowed operands. Struct/enum `==` uses structural
  equality. `method operator_index(self: &MyType, i: Uint) -> &T` enables
  indexing as a readable/writable place.

## Modules and visibility

Imports are relative to the importing file:
`import {Point, make_point} from "helper.solar";`,
`import helper from "helper.solar";` (then `helper::Point`, `helper::make_point`),
or `import * from "helper.solar";`. Named imports can use paths like
`{helper::Point}`. `pub import ...` re-exports. Items and fields are private
unless marked `pub`; mark both a struct and its externally accessed fields.
Associated functions belong in the type's defining file. A method must have a
type defined in its file in at least one parameter; use a wrapper type to extend
foreign APIs.

## Standard library and failures

The standard prelude is implicit. Common tested APIs:

- `println("text"&)`, `write_stdout(bytes)`, `println(n)` and `n.to_string()`
  for `Int`/`Uint`; convert other numbers explicitly. `assert(condition)`.
- `Option#[T]::{Some(T), None}`; `option.is_some()`. Handle values with `match`.
- `let v = vec::Vec([]#[Int]&); v&.push(1); v&.at(0u)@ = 2;`
  `v&.as_slice()` returns `&[T]`; `v&.iter()` yields element references.
  Slice iterators support `.map(\x: &Int x@ * 2)` and `.filter(...)`.
  Consume these iterators through `next()`:

  ```solar
  fn main() {
      let it = [1, 2]&.iter().map(\x: &Int x@ * 2);
      loop {
          match it&.next() {
              Option#[Int]::Some(n) => println(n),
              Option#[Int]::None => { break; },
          };
      }
  }
  ```

- `let m = hashbrown::HashMap#[Int, Int]();`: `m&.insert(k, v)` returns
  `Option#[V]`; `m&.get(k&)` returns `Option#[&V]`; `remove`, `len`,
  `contains_key`, `clear`, `for_each(\k: &Int, v: &Int { ... })`.
- `file::open("path"&, read = true, write = false)` returns `FileDesc`.
  For output use `create = true, truncate = true`. `fd.read(4096u)` returns
  bytes; `fd.read(buffer&)` returns a byte count; `fd.write(bytes&)`, `fd.close()`.
  Standard streams: `file::stdin()`, `stdout()`, `stderr()`.
- `process::args()` and `process::env()` return `&[&[Uint8]]`.
  `math::sqrt(2f)`, `math::PI`; `thread::spawn(\ { ... })`, `handle.join()`,
  `thread::sleep(nanoseconds)` (`Uint64`). `sync::Mutex()` has `lock`/`unlock`;
  `sync::Channel#[Int]()` has `send`/`recv` on references.
- `try { work(); } catch (e) { println(e.message); }` catches an `Exception`;
  optional annotation: `catch (e: Exception)`. Throw with `throw("message"&)`,
  rethrow with `throw(e)`. Runtime arithmetic, bounds, and null failures are
  catchable. Typed payloads: `throw("message"&, Any(value&))`, then
  `e.payload.downcast#[MyType]()` returns `&?MyType`.

## Advanced features

`match.reflect T { "struct" => ..., "enum" => ..., _ => ... }` selects a
compile-time type-kind branch; unselected branches are not type-checked.
`for.reflect_fields (name, field) in obj& { ... }` unrolls over struct fields
with `name: &[Uint8]`, `field: &FieldType`.
`match.reflect_variant (name, index, value) in obj& { ... }` dispatches on an
enum, with a `Uint` variant index and payload value; do not use `value` in a
body that must also work for unit variants. Paired forms use
`for.reflect_fields_pair (name, a, b) in (x&, y&)` and
`match.reflect_variant_pair (name, index, a, b) in (x&, y&)` for the same type;
the latter runs only for matching variants.

Unsafe operations require `unsafe { ... }`, including inside an `unsafe fn`
or `unsafe method`; nested closures need their own unsafe block. Low-level
intrinsics require explicit imports such as
`import intrinsics from "@intrinsics";`. Use library wrappers for ordinary I/O,
collections, and threading.
