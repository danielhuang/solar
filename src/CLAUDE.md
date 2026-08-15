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
- Every method signature must mention a parameter type owned by its declaring
  file. The standard library may additionally define methods on primitive and
  structural built-in types.
- `ast::PRIMITIVE_TYPES` is the single registry of primitive type names; name
  parsing, numeric-constructor generation, and coherence checks derive from it.
- Eagerly lower concrete top-level functions and methods so their bodies are
  validated and typed tooling can inspect them without requiring a call site.
  Generic declarations remain demand-monomorphized.
- Keep user-written and compiler-generated local identifiers as distinct
  `Ident` variants until `mangled_ast` renders them into disjoint strings.
- Keep mangling and `solar-system/src/panic.rs` demangling in sync.
- User program errors must use `CompileError` and the reporting system in
  `error.rs`; malformed input must not panic the compiler or LSP.
- Escape analysis is conservative: uncertainty means the value may escape.
- Compile-time field reflection evaluates its object once.
- Surface `for binding in value` is duck-typed: desugaring evaluates `value`
  once, calls `iter`, then drives `next` inside a `loop` until it returns
  `Option::None`.
- `static(thread_local)` gives each native thread an independently initialized
  stable cell. Its literal initializer is replayed when a spawned Solar thread
  starts; references to the cell may cross threads and outlive the owner.
- Solar copies may alias. Every backend must implement memmove semantics,
  including aggregate and slice-range copies.
- Pointer-bearing values must reach LLVM with pointer-typed pointer words so
  the write-barrier pass can distinguish references from scalar data.

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
Nested parentheses containing a single multiline block value hug that value at
every layer, leaving the block's contents to begin on the following line.
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
