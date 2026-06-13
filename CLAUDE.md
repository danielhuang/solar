# Solar

Solar is a custom programming language. The compiler is written in Rust (edition 2024).

## Dev environment setup

Required tools:
- **Rust** (via rustup)
- **LLVM and clang** — version must match rustc's LLVM version (`rustc --version --verbose | grep LLVM`)
- **lld** — same version as LLVM/clang
- **Node.js** — needed by tree-sitter to generate the parser
- **tree-sitter CLI** — `cargo install tree-sitter-cli`

The unversioned commands (`clang`, `llvm-as`, `llvm-link`, `llc`, `opt`, `ld.lld`) must be on `$PATH`.

## Build

```
cargo build
```

## Run

```bash
# uses IR interpreter, does not support multithreaded features
cargo run -- examples/example.solar

# script adds path and file extension, this runs `examples/my_file.solar`
./run_example.sh my_file

# compile and run any solar file at any path
cargo build --release -p solar-system # build the runtime libaries for linking in release mode, since the next command will use the optimized version
cargo run --bin compile -- my_path/my_file.solar target/my_executable # omits `--release` to make rustc faster, but the solar compiler will run in release mode
target/my_executable # execute the new program

# compile and run in debug mode (enables asan)
cargo run --bin run_codegen -- my_path/my_file.solar
```

The intended use is to compile in release mode using `--bin compile`, any other uses are for debugging.

### Diagnostic flags (environment variables)

These flags only apply to compiled binaries, and not the interpreter.

- `SOLAR_PRINT_GC_STATS=1` - prints stats for each GC cycle
- `SOLAR_PRINT_ALLOCS=1` - prints EVERY allocation; this may flood the output, so use sparingly!
- `SOLAR_DISABLE_GC=1` - disables GC and switches to a bump allocator

## Changing syntax rules

When modifying the grammar, follow all of these steps:

1. Edit `tree-sitter-solar/grammar.js`
2. Update `src/parser.rs` — CST-to-AST conversion (field names, node kinds)
3. Update `examples/example.solar` to use the new syntax
4. Build and test: `cargo build && cargo run -- examples/example.solar`

The C parser is regenerated automatically by `tree-sitter-solar/build.rs` when `grammar.js` changes. Do not edit `tree-sitter-solar/src/` by hand — it is generated and gitignored.

## Pipeline

The pipeline is orchestrated through `src/pipeline.rs` with typed stages and method chaining:

```
pipeline::compile(path) → Typed → .to_ir() → Ir → .to_c(name) → CSource → .to_binary(name, mode) → Binary → .run(name) → String
```

Entry point is `pipeline::compile(file_path)` which returns a `Typed` struct. Each stage wraps its data and has methods to advance to the next stage. You can stop at any stage (e.g., stop at `Typed` for typecheck tests, at `Ir` for interpreter tests).

`CSource::to_binary` supports two modes via `CompileMode::Debug` (ASAN + clang, links `target/debug/libsolar_system`) and `CompileMode::Release` (LLVM LTO, cross-language optimization, allocator attribute stamping, links `target/release/libsolar_system.a`). Intermediate files go in `target/solar/{name}_{random_hex}/` and are kept for debugging.

In release mode, GC write barriers are inserted by `src/write_barriers.rs` as a textual rewrite of the LLVM IR *after* `opt -O3` (so barrier calls don't block allocation elision/SROA); the final `clang -O3` link inlines the barrier fast path. The barrier (`sol_write_barrier` in `solar-system/src/gc.rs`) is a Dijkstra-style insertion barrier gated on `SOL_CONCURRENT_MARKING`, which is never set while the GC is stop-the-world — so it is currently a no-op flag check, kept as the foundation for concurrent marking.

### Stage details

1. **Parse**: tree-sitter produces a CST, `parser.rs` converts it to an untyped `ast::SourceFile`.
   1b. **Resolve**: `resolve::resolve` recursively parses imported files, validates exports/visibility, and rewrites all ASTs into a single unified `ast::SourceFile` with module-mangled names (e.g., `__mod_foo__Point`). Root file items keep their original names. The stdlib is parsed in the same resolver and every user file gets a synthetic `import * from "@std"` — stdlib pub items (print_int, etc.) are available directly, and pub module re-exports (e.g., `pub import vec from "vec.solar"` in lib.solar) become module aliases (e.g., `vec::Vec`). Wildcard imports propagate pub module re-exports from the source file. Import statements are stripped. Returns `(SourceFile, SourceMap)` for multi-file error reporting.
2. **Type check / lower**: `typed_ast::lower` walks the untyped AST, infers and checks types, and produces a `typed_ast::SourceFile` where every `Expr` carries a concrete `Type`. Panics on type errors. Closures are desugared into synthetic functions (`__closure_N`) with capture analysis; the `Closure` ExprKind records the synthetic function name and captured variables. Methods are desugared into regular functions with mangled names (`__method_{name}_{type}`); the receiver becomes the first argument. Generic structs/enums are monomorphized: `Box#[Int]` becomes `Box_Int`, `Option#[&Node]` becomes `Option_ref_Node`. Destructuring patterns in `let` bindings and function parameters are desugared into temp variables + field accesses/indexing; downstream layers see only simple `let` statements. Compile-time reflection (`match.reflect Type { "struct" => ..., "enum" => ..., _ => ... }`) is resolved here: the inspected type is classified at compile time, the taken branch replaces the whole expression, and non-taken branches are erased without being type-checked. `for.reflect_fields x in o` (where `o: &T`, `T` a struct) unrolls its body into one scoped block per field with `x: (&[Uint8], &F)` — field name and value reference. `match.reflect_variant (variant, val) in o` (where `o: &T`, `T` an enum) desugars into a match over the enum with the body duplicated in every arm, binding the pattern against `(&[Uint8], Payload)` — variant name and payload by value (a bare name binds the whole tuple; unit-variant arms bind only the name part of a `(variant, val)` pattern). Downstream layers see only concrete mangled names.
3. **IR lower**: `ir::lower` converts the typed AST into a flat-tree IR. Variable names are erased to globally unique `VarId`s. Struct types get memory layouts (field offsets, sizes, alignment). Function bodies become flat `Vec<Node>` where children are referenced by `NodeId` index. Function values are 16 bytes (code pointer + env pointer). All Solar functions receive a hidden `__env` parameter; non-closures pass/ignore 0.
4. **Interpret (IR)**: `ir_interp::interpret` executes the IR using flat memory (`BTreeMap<address, u64>`). Structs are decomposed into per-field entries at layout offsets. Arrays allocate separate element storage (ptr+len). Refs are plain addresses. Unique pointers (`^T`) are also plain addresses but use type-aware deep copy: when a value containing `^T` is copied, the pointee is recursively cloned into fresh memory (unlike `&T` which copies only the pointer). All other copies are memcpy.
5. **Interpret (AST)**: `ast_interp::interpret` walks the typed AST directly and executes it using `Rc<RefCell<Value>>` slots. Kept as a reference interpreter; runtime tests assert both interpreters produce identical output.
6. **Compile (IR)**: `codegen::generate` lowers the IR into C code which is linked to `solar-system` to compile the program as a native executable. Runtime tests assert that the compiled program produces identical output as the interpreters.

## Project structure

- `src` — Main code
- `src/std/` — Standard library; `lib.solar` is the entry point, can import other files in this directory
- `tree-sitter-solar` — tree-sitter grammar: main file is in `grammar.js`
- `solar-system` — native library linked to compiled programs
- `examples` — example programs, main program is `example.solar
- `tests` — integration tests (runtime tests run both interpreters, typecheck error cases)

## Conventions

- Prefer `unwrap()`/`assert!()` over `process::exit()` — let panics handle errors.
- Rust edition 2024: use `unsafe extern "C"` blocks, not `extern "C"`.

## Workflow

- If there are any changes that make CLAUDE.md outdated, update CLAUDE.md before making any commits.
- Before making a commit, do `cargo fmt`
- Run `cargo clippy --all` (need to check sub-crates) instead of `cargo check`
