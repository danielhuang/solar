# Compiler

## Pipeline

`src/pipeline.rs` owns the typed stage pipeline:

```text
compile(path) -> Typed -> Mangled -> Ir -> CSource -> Binary
```

- `parser.rs` converts tree-sitter CST nodes to `ast`.
- `resolve.rs` loads imports, checks visibility, and assigns structural
  definition provenance.
- `typed_ast.rs` type-checks, resolves overloads, and monomorphizes.
- `mangled_ast.rs` is the only stage that renders structural identities into
  final symbol strings.
- `ir.rs` lowers the mangled AST to flat IR.
- `ir_opt.rs` performs conservative escape analysis.
- `codegen.rs` emits C.

Both execution paths pass through `Mangled`: `ast_interp` consumes the mangled
AST, while `ir_interp` and native codegen consume IR.

## Correctness constraints

- Keep identities structural through resolution and type checking. Do not
  encode file provenance in names before `mangled_ast`.
- Cross-file function references must reach type checking as provenance-bearing
  `GlobalRef` nodes. Bare-name fallback is reserved for same-file raw
  type-checking and synthetic numeric constructors; it must not search the
  transitive import graph by spelling.
- `types::Type<I>` is shared by the typed and mangled stages: typed types use
  structural `TypeId` identities, while mangled types use final symbol strings.
- Every method signature must mention a parameter type owned by its declaring
  file. The standard library may additionally define methods on primitive and
  structural built-in types.
- `ast::PRIMITIVE_TYPES` is the single registry of primitive type names; name
  parsing, numeric-constructor generation, and coherence checks derive from it.
- Eagerly lower concrete top-level functions and methods so their bodies are
  validated and typed tooling can inspect them without requiring a call site.
  Generic declarations remain demand-monomorphized.
- Function and method type parameters declared with `out` follow all inferred
  type parameters and are supplied by the call site's `#[...]` list. Non-`out`
  parameters cannot be written at a call site: they are always inferred and
  must occur in at least one parameter type. Output parameters need not occur
  in the signature.
- `size_of#[T]()` accepts only sized types. Each concrete type produces a
  zero-argument monomorphized function whose constant body uses the same size
  and alignment rules as IR layout.
- `black_box_ref(&T)` accepts a sized reference, passes its pointer through the
  native runtime's Rust optimizer barrier, and never retains the reference.
  Escape analysis must therefore treat its argument as non-escaping.
- `gc_keepalive(&T)` accepts sized and unsized references and forces the data
  pointer through a non-inlined native assembly barrier so the conservative
  collector can find it in a register or on the stack through that call. It
  does not retain the reference, so escape analysis treats its argument as
  non-escaping.
- `Any` is a sized, reference-like 16-byte `(pointer, private type tag)` value.
  It accepts only references to sized types, aliases its referent when copied,
  and copies/loads/stores the pair with the tear-free unordered i128 helpers.
  Its numeric tag is never part of the Solar API; bits 48..63 are `0x00FF` so
  conservative stack scans are unlikely to mistake the metadata for a pointer.
- Keep user-written and compiler-generated local identifiers as distinct
  `Ident` variants until `mangled_ast` renders them into disjoint strings.
- Keep mangling and `solar-system/src/panic.rs` demangling in sync.
- User program errors must use `CompileError` and the reporting system in
  `error.rs`; malformed input must not panic the compiler or LSP.
- Escape analysis is conservative: uncertainty means the value may escape.
- Compile-time field reflection evaluates its object once.
- Keep integer matches as flat match nodes through typed AST, mangling, and IR;
  expanding their arms into nested `if` expressions makes compiler stack usage
  proportional to the number of arms.
- Surface `for binding in value` is duck-typed: desugaring evaluates `value`
  once, calls `iter`, then drives `next` inside a `loop` until it returns
  `Option::None`.
- `static(thread_local)` gives each native thread an independently initialized
  stable cell. Its literal initializer is replayed when a spawned Solar thread
  starts; references to the cell may cross threads and outlive the owner.
- Solar copies may alias. Every backend must implement memmove semantics,
  including aggregate and slice-range copies.
- IR layouts pack ordinary struct fields and disjoint enum payload slots into
  alignment gaps while preserving each field's alignment. Declaration and
  discriminant order remain semantic order; an unsized struct field remains the
  declared and physical tail. `struct(repr(C))` instead preserves declaration
  order and applies C field alignment and tail padding. Its by-value fields must
  be C-representable: nested structs must also use `repr(C)`, and enums,
  zero-sized or unsized types, fat pointers, closure values, and runtime-only
  handles are rejected.
- Pointer-bearing values must reach LLVM with pointer-typed pointer words so
  the write-barrier pass can distinguish references from scalar data.
- `CompileOptions` controls GC, GC-San, and optimization independently. GC-San
  and optimization require GC; `-O3` and LTO are used only when optimization is
  enabled.
- Function values are always safe to call. Accessing an `unsafe fn` or
  `unsafe method`, including converting a declaration to a function value,
  requires an explicit `unsafe {}` block. An unsafe declaration does not make
  its own body, or a closure created inside an unsafe block, implicitly unsafe.
- `syscall` is a native-only unsafe intrinsic with an `Int64`/`Uint64` syscall
  number, up to six `Int64`, `Uint64`, or reference arguments, and an `Int64`
  raw kernel result: failures are negative errno values. `fd_from_raw(Int32)`
  is unsafe and transfers descriptor ownership to the GC;
  `fd_to_raw(FileDesc)` safely borrows the underlying number.

## LSP

`src/bin/lsp.rs` shares one resolved-symbol path between hover and
go-to-definition. Both features must identify the same definition or overload
set. Calls in unresolved generic bodies may return every viable overload.
Do not add spelling-based definition fallbacks.
Go-to-definition on a binary operator follows the `operator_*` method selected
by type checking; primitive operators have no definition target.
Hover renders concrete signatures for compiler-provided intrinsics, numeric
constructors, and primitive operators, marks them `built-in`, and does not
manufacture source locations for them.

The LSP treats the open file as the compilation root for diagnostics. Convert
compiler failures into diagnostics rather than terminating the server.
Diagnostics and language features share one resolved and type-checked analysis
per document revision; do not compile the same buffer independently for each
request.
Inlay hints show compiler-inferred binding and non-unit return types only where
the source annotation is omitted. Their positions use LSP UTF-16 coordinates
and range requests must not return hints outside the requested range.
Completion is type-aware for member access. Method candidates must match the
compiler's explicit receiver rules and add postfix `&` or `@` edits when a
value must be referenced or a reference dereferenced to match `self`. When the
receiver binds every inferred type parameter of a generic method, completion
must also reject candidates whose resulting monomorphized body does not
type-check.
Top-level completion must follow the root file's imports and public re-export
chains; loading a private transitive module does not put its declarations in
scope.

## Formatter

Run `cargo run --bin fmt -- path/to/file.solar` to rewrite Solar source files.
The formatter uses an 80-column layout, tabs for indentation, compact blocks
when written compact and they fit, and trailing commas only for multiline
lists. It preserves comments, collapses consecutive blank lines, and refuses to
modify files with parse errors. Files end with exactly one newline. Formatting
uses the compiler's semantic AST for declarations and expressions while
`fmt/syntax.rs` retains comments, blank lines, redundant grouping, and exact
brace spans needed only for source layout. Semantic spans recover literal
spellings from the source.
Blank lines immediately inside braces, parentheses, and brackets are removed.
Elsewhere, blank lines are retained only between items at the top level, between
statements in blocks, or between list elements. List separators remain trailing
before a retained blank line.
Only multiline block items require surrounding blank lines; compact block items
can remain adjacent to other items. Nonempty executable blocks remain multiline
when written multiline, even with only one code item, while short declaration
and parameter lists still collapse.
Nested parentheses and arrays containing a single multiline block value hug
that value at every layer, leaving the block's contents to begin on the
following line. Block-bodied closures and plain block expressions are huggable;
control-flow expressions are not.
Zero-parameter closures have a space after `\`; parameterized closures keep the
first parameter adjacent to it.
Range operators have no surrounding spaces (`a..b`).
Binary `&` and `^` have surrounding spaces, while their postfix reference and
unique forms remain attached to their operands.
The language server advertises whole-document formatting so editors can use the
same formatter for format-on-save.

## Validation

Use focused tests while iterating, then run:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
