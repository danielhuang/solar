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
Completion and inlay hints retain resolved type facts from the last successful
document revision when the current revision does not type-check. Diagnostics,
navigation, hover, and semantic highlighting remain tied to the current revision.
Completion is type-aware for member access. Method candidates must match the
compiler's explicit receiver rules and add postfix `&` or `@` edits when a
value must be referenced or a reference dereferenced to match `self`. When the
receiver binds every inferred type parameter of a generic method, completion
must also reject candidates whose resulting monomorphized body does not
type-check.
Completion after a module path such as `process::` lists that namespace's public
declarations and public submodules, including named and module re-exports.
Signature help inside function and method argument lists includes every overload
whose parameter prefix can still accept the completed arguments, and highlights
the active parameter (accounting for a method's implicit `self` argument).
When a type-aware completion probe fails, retry after successively promoting the
cursor's enclosing block contents into their parent block, stopping at the
function boundary. Probe rewrites must preserve source positions.
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
