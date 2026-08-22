# Solar

Solar is a programming language whose compiler is written in Rust 2024.

## Toolchain

- Install Rust with rustup.
- LLVM, clang, clang++, lld, and the LLVM development headers must match the
  LLVM version used by rustc.
- Native codegen expects unversioned `clang`, `clang++`, `llvm-as`,
  `llvm-link`, `opt`, `ld.lld`, and `llvm-config` commands on `PATH`.
- Grammar development also requires Node.js and the tree-sitter CLI.

## Common commands

```bash
cargo build
cargo nextest run --workspace # do not use plain `cargo test`
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Run a program with the IR interpreter:

```bash
cargo run -- examples/example.solar
```

Compile a native release binary:

```bash
cargo build --release -p solar-system
cargo run --bin compile -- path/to/program.solar target/program
```

Build a debug ASAN binary:

```bash
cargo run --bin run_codegen -- path/to/program.solar
```

Release codegen is the production path. Debug codegen and the interpreters are
diagnostic backends.

## Components

Read the guide for the component being changed:

- [Compiler pipeline](src/CLAUDE.md)
- [Standard library](src/std/CLAUDE.md)
- [Grammar](tree-sitter-solar/CLAUDE.md)
- [LLVM passes](llvm-pass/CLAUDE.md)
- [Native runtime](solar-system/CLAUDE.md)
- [Tests](tests/CLAUDE.md)

## Performance

Performance benchmarks are in `bench/` and contain comparisons with other languages.

## Repository conventions

- Prefer `unwrap()` and `assert!()` to manually printing an error and exiting.
- Rust 2024 requires `unsafe extern "C"` blocks.
- Preserve unrelated work in a dirty worktree.
- Keep public Rust and Solar APIs documented.
- Update the relevant component guide when a durable invariant or workflow
  changes. Do not add bug history or implementation diaries.
- Before committing, run formatting, Clippy, and tests appropriate to the
  changed components.
