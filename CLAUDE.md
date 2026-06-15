# Solar

Solar is a custom programming language. The compiler is written in Rust (edition 2024).

## Dev environment setup

Required tools:
- **Rust** (via rustup)
- **LLVM and clang** — version must match rustc's LLVM version (`rustc --version --verbose | grep LLVM`). The distro repos may lag; the matching version usually comes from the official LLVM apt repo (`apt.llvm.org`, e.g. `llvm-toolchain-trixie-22`).
- **LLVM dev headers + `clang++` and `llvm-config`** — `build.rs` compiles the GC write-barrier LLVM pass plugin (`llvm-pass/SolarWriteBarriers.cpp`) against these. Install e.g. `llvm-22-dev`. Without them `cargo build` still works (interpreter only) but native codegen fails with a clear message.
- **lld** — same version as LLVM/clang
- **Node.js** — needed by tree-sitter to generate the parser
- **tree-sitter CLI** — `cargo install tree-sitter-cli`

The unversioned commands (`clang`, `clang++`, `llvm-as`, `llvm-link`, `llc`, `opt`, `ld.lld`, `llvm-config`) must be on `$PATH`.

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

GC write barriers are inserted by a real LLVM pass plugin (`llvm-pass/SolarWriteBarriers.cpp`), built by `build.rs`. The plugin provides two passes run via `opt -load-pass-plugin=…`:

- **`solar-lower-gc-alloc`** (release only, *before* `opt -O3`): rewrites each generated `sol_alloc(size, align, mark_fn)` call into a recognized `calloc(1, size)` carrying `(align, mark_fn)` in `!solar.alloc` instruction metadata, and each `sol_memcpy` into `llvm.memcpy`. This is the key to allocation elision: LLVM's allocation-promotion/dead-alloc transforms key on *recognized* allocators (TargetLibraryInfo), so they SROA/elide a non-escaping `calloc` but **not** a custom `sol_alloc`, even one stamped with the full malloc attribute set. `calloc` (not `malloc`) preserves `sol_alloc`'s zeroing so the optimizer never sees uninitialized reads. The referenced `_mark_*` functions are anchored in `llvm.compiler.used` so they survive globaldce until raising.
- **`solar-write-barriers`** (debug + release, *after* `opt -O3`): first *raises* surviving `calloc … !solar.alloc` calls back to `sol_alloc` (reading the metadata), then inserts the barriers. It instruments `store ptr` (and `<N x ptr>` vector stores → bulk barrier) into non-stack/global destinations, plus aggregate copies — `llvm.memcpy`/`memmove` and any residual `sol_memcpy` call — via `sol_gc_memcpy_barrier`. `getOrInsertFunction` declares the barrier runtime functions when the module doesn't define them.

Inserting barriers as IR (not text — replacing an earlier `llvm-dis | edit | llvm-as` rewrite) gives robust stack-vs-heap provenance via `getUnderlyingObject`, native debug-location propagation (the inserted call inherits the store's `!dbg`, so LLVM never strips module debug info — which had silently dropped solar-system DWARF for profilers), and type safety. Both barrier-relevant runtime side effects are kept out of the optimizer's view until *after* elision: barriers are inserted post-`opt`, and `sol_memcpy` is a **plain copy** (no inline shading — that would make freshly-allocated objects escape and block their elision; the shading is re-added by the post-opt memcpy barrier). In release the final `clang -O3` link inlines the barrier fast path; in debug the pass runs on the `-emit-llvm` bitcode before the (LTO-deferred) ASAN instrumentation, so debug/ASAN test binaries also exercise the collector with barriers active.

The collector (`solar-system/src/gc.rs`) is **concurrent**: a dedicated GC thread spawned in `sol_start` owns every cycle. Mutators never collect — on the heap-growth heuristic `sol_alloc` calls `request_gc` (a futex wake) and keeps running. A cycle is: **STW pause 1** (signal all registered mutators, scan their stacks+registers, seed the gray queue with root pointer values, set `SOL_CONCURRENT_MARKING`, resume) → **concurrent parallel mark** (`parallel_mark` submits one job per thread-pool worker; workers drain the gray queue, follow heap edges, and overflow excess back to the queue for idle workers to steal, until quiescence — while mutators run, the Dijkstra insertion barrier and the inserted aggregate-copy barrier (`sol_gc_memcpy_barrier`) shade newly-stored pointers, and new allocations are *born black*) → **STW pause 2** (clear the flag, flush gray buffers + re-scan roots, drain to fixpoint, then the parallel sweep). The gray frontier is a **sharded** queue (`GRAY[N]` + per-thread buffers in `ThreadSlot`) kept separate from the alloc/mark bitmaps; consumers swap a whole shard in O(1). The barrier does **white-only shading** (skips already-marked targets via `heap::is_marked_addr`) so a fast allocator's born-black stores don't flood the queue. **Allocation back-pressure**: `ALLOCATED_SINCE_GC` (batched per-thread) is capped at `alloc_hard_cap()` (a floor, else scaled by *traced* live — live excluding this cycle's born-black float, which avoids a float→cap→float feedback loop); over the cap, `sol_alloc` stalls as a safepoint (`stall_for_gc`) until a cycle reclaims space. Remaining caveat: marker heap reads are racy-by-design (sound on x86 for aligned words).

**GC-managed file descriptors** (`solar-system/src/file.rs`): the built-in `FileDesc` type is an opaque handle with the byte representation of `&Int32` — a traced pointer into a separate **fd arena** (a 4 GiB `PROT_NONE` mmap that is *never read or written*; the fd number is recovered arithmetically as `addr - FD_BASE`). The `file_open(path, flags: Int, mode: Uint)` intrinsic (defined in `src/std/file.solar` and re-exported from `lib.solar`) calls `sol_file_open`, which `open(2)`s the file with the given flags (always OR-ing in `O_CLOEXEC`) and mode and returns `FD_BASE + fd`, setting the fd's bit in a side alloc bitmap. The `flags`/`mode` are an implementation detail of `@std`: `file::open(path, read=true, write=true, append=false, truncate=false, new=false, create=false, mode=0o666u)` takes boolean keyword args and builds the `open(2)` flag word from `O_*` bit constants declared in its own body (`new` → `O_EXCL`); `mode` (the creation permission bits) is a keyword arg defaulting to 0o666 (a second mark bitmap and an `FD_HWM` mirror the heap's bitmaps). The collector traces `FileDesc`s like any pointer: `plausible()` accepts fd-arena addresses, the generated mark function (`_mark_single_ptr`) enqueues the value, `drain` routes it to `file::fd_mark`, and the write barriers shade fd-arena stores (white-only via `file::is_marked`). At STW pause 2, `file::fd_sweep` runs alongside the heap sweep and `close()`s every fd whose slot is allocated-but-unmarked — i.e. no live `FileDesc` remains. Born-marked-during-mark mirrors born-black. The `file_close` intrinsic (`file::close(f)` in `@std`) calls `sol_file_close`, which does *not* `close()` the fd (that would free the number, letting a later `open` reuse it and a stale `FileDesc` silently alias the new file). Instead it `dup2`s a process-wide **dead fd** — the read end of a pipe whose write end is closed at startup, so reads return EOF — over the fd: the underlying file is released now, but the number stays occupied (and its alloc bit set, so the collector still traces and eventually reclaims it). The `file_stdin`/`file_stdout` intrinsics (`file::stdin()`/`file::stdout()` in `@std`) return a `FileDesc` for the process's standard streams (`sol_file_stdin`/`sol_file_stdout` return `FD_BASE + 0`/`FD_BASE + 1`); these are deliberately **not** registered in the alloc bitmap, so the collector traces them harmlessly but `fd_sweep` (which only closes allocated-but-unmarked fds) never auto-closes them. The `file_read(fd, dst: &[Uint8]) -> Uint`/`file_write_partial(fd, src: &[Uint8]) -> Uint` intrinsics (`sol_file_read`/`sol_file_write_partial`) call `read(2)`/`write(2)` directly (single, possibly-partial syscall, retried on `EINTR`, panic on other I/O error), and back the `FileDesc` methods in `file.solar`: `read(buf)` / `read(max_size)` (the latter allocates a GC buffer, reads once, and returns the bytes read as a `&[Uint8]` slice into it), `write_partial(src)`, and `write(src)` (loops `write_partial` to a full write, like Rust's `write_all`). The interpreters have no fd arena/collector: a `FileDesc` is a plain integer index into a per-run virtual table (`src/interp_io.rs`, `FileTable` of `Box<dyn Read + Write>`) — index 0 is stdin, 1 is stdout (wrapped so the unsupported half panics), and `file_open` decodes the flag word into `OpenOptions` and pushes the boxed `File`, returning its index. `write_stdout`/`read_stdin`/`file_read`/`file_write_partial` all route through this table; `interpret_to` takes both a `stdin` and `stdout`. `close` is a no-op in the interpreters (auto-close and dead-fd neutering are compiled-runtime only).

### Stage details

1. **Parse**: tree-sitter produces a CST, `parser.rs` converts it to an untyped `ast::SourceFile`.
   1b. **Resolve**: `resolve::resolve` recursively parses imported files, validates exports/visibility, and rewrites all ASTs into a single unified `ast::SourceFile` with module-mangled names (e.g., `__mod_foo__Point`). Root file items keep their original names. The stdlib is parsed in the same resolver and every user file gets a synthetic `import * from "@std"` — stdlib pub items (print_int, etc.) are available directly, and pub module re-exports (e.g., `pub import vec from "vec.solar"` in lib.solar) become module aliases (e.g., `vec::Vec`). Wildcard imports propagate pub module re-exports from the source file. Import statements are stripped. Returns `(SourceFile, SourceMap)` for multi-file error reporting.
2. **Type check / lower**: `typed_ast::lower` walks the untyped AST, infers and checks types, and produces a `typed_ast::SourceFile` where every `Expr` carries a concrete `Type`. Panics on type errors. Closures are desugared into synthetic functions (`__closure_N`) with capture analysis; the `Closure` ExprKind records the synthetic function name and captured variables. Methods are desugared into regular functions with mangled names (`__method_{name}_{type}`); the receiver becomes the first argument. **Optional keyword parameters** — `fn f(normal: Int, kw1: Int = 0, kw2 = 1u)` — must follow all normal parameters; their defaults must be literals (ints, bools, arrays, strings-as-arrays, and `&`/`^` of those), and an omitted type is inferred from the default (`prepare_keyword_params`, at function/method registration, bakes the inferred type and validates). They are **invisible to overload resolution** (overloads are matched on the required, non-default parameters only) and **keyword-only** at call sites (`f(1, kw1=2)` in `ast::ExprKind::Call`/`MethodCall`'s `kwargs`). `resolve_overloaded_call` expands each call into a fully positional argument list — provided kwargs by name, unspecified optionals from their default literal — so the lowered `typed_ast`/IR sees only ordinary positional calls (no kwarg concept downstream). Keyword args are rejected on non-registry calls (enum construction, nested, indirect). **Const declarations** (`const NAME [: T] = <literal>`, `ast::TopLevelItem::Const` / `StatementKind::Const`) bind a name to a literal value; they may be top-level (exported/imported like other items via `resolve`, module-mangled) or local to a block (scoped), and the type is optional (the explicit type, if given, coerces the literal). Each use site is **substituted with the (re-lowered) literal value** during `Identifier` lowering — innermost local const scope first (`const_scopes`, pushed/popped with `push_scope`/`pop_scope`), then top-level `consts` — so the lowered output contains no const declarations. The value must be a literal (`is_literal_default`). Generic structs/enums are monomorphized: `Box#[Int]` becomes `Box_Int`, `Option#[&Node]` becomes `Option_ref_Node`. **Nullable references** `&?T` are a distinct type (`Type::NullableRef`/`NullableRefUnsized`, mangled `ref_nullable_`) with the same representation as `&T` (8-byte pointer, or 16-byte fat pointer for unsized `T`); their null value is written `null#[T]` (always with the type argument; `ExprKind::NullLiteral` → IR `NodeKind::Null` → a zero pointer). A normal `&T` implicitly coerces to `&?T` (a no-op retag in `try_coerce`); the reverse is **not** implicit — to get a `&T` from a `&?T`, write `nullable_ref@&` (the `@` null-checks and yields the pointee place, `&` re-borrows). Dereferencing `&?T` with `@` emits a null check (`sol_null_check` in compiled code, `assert!` in the interpreters; panic message `null pointer dereference`); `&T` deref is unchecked. `==`/`!=` compare a nullable ref against `null#[T]` (or another ref, at least one side nullable) by pointer identity. Destructuring patterns in `let` bindings and function parameters are desugared into temp variables + field accesses/indexing; downstream layers see only simple `let` statements. Compile-time reflection (`match.reflect Type { "struct" => ..., "enum" => ..., _ => ... }`) is resolved here: the inspected type is classified at compile time, the taken branch replaces the whole expression, and non-taken branches are erased without being type-checked. `for.reflect_fields x in o` (where `o: &T`, `T` a struct) unrolls its body into one scoped block per field with `x: (&[Uint8], &F)` — field name and value reference. `match.reflect_variant (variant, val) in o` (where `o: &T`, `T` an enum) desugars into a match over the enum with the body duplicated in every arm, binding the pattern against `(&[Uint8], Payload)` — variant name and payload by value (a bare name binds the whole tuple; unit-variant arms bind only the name part of a `(variant, val)` pattern). Downstream layers see only concrete mangled names.
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
