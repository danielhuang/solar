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

## Invariants

- Keep identities structural through resolution and type checking. Do not
  encode file provenance in names before `mangled_ast`.
- Keep user-written and compiler-generated local identifiers as distinct
  `Ident` variants until `mangled_ast` renders them into disjoint strings.
- Keep mangling and `solar-system/src/panic.rs` demangling in sync.
- User program errors must use `CompileError` and the reporting system in
  `error.rs`; malformed input must not panic the compiler or LSP.
- Escape analysis is conservative: uncertainty means the value may escape.
- Escape analysis follows place roots, not value-only index or slice-bound
  operands. An interior reference derived through a dereference is non-escaping
  only when the derived reference is itself contained.
- Compile-time field reflection evaluates its object once. Simple tuple
  patterns bind generated components directly, and wildcard components may be
  omitted because those generated name/reference expressions are pure.
- Solar copies may alias. Every backend must implement memmove semantics,
  including aggregate and slice-range copies.
- Pointer-bearing values must reach LLVM with pointer-typed pointer words so
  the write-barrier pass can distinguish references from scalar data.
- Generated C is compiled with `-fno-strict-aliasing`.

## LSP

`src/bin/lsp.rs` shares one resolved-symbol path between hover and
go-to-definition. Both features must identify the same definition or overload
set. Calls in unresolved generic bodies may return every viable overload.
Do not add spelling-based definition fallbacks.

The LSP treats the open file as the compilation root for diagnostics. Convert
compiler failures into diagnostics rather than terminating the server.
Diagnostics and language features share one resolved and type-checked analysis
per document revision; do not compile the same buffer independently for each
request.

## Validation

Use focused tests while iterating, then run:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
