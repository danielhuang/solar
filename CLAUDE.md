# Solar

## Development environment

- Install Rust with rustup.
- LLVM, clang, clang++, lld, and the LLVM development headers must match the
  LLVM version used by rustc.
- Provide unversioned `clang`, `clang++`, `llvm-as`, `llvm-link`, `opt`,
  `ld.lld`, and `llvm-config` commands on `PATH`.
- Grammar development requires Node.js and the tree-sitter CLI.

## Project conventions

- Treat release codegen as the production path; debug codegen and interpreters
  are diagnostic backends.
- Prefer `unwrap()` and `assert!()` to manually printing an error and exiting.
- Preserve unrelated work in a dirty worktree.
- Keep public Rust and Solar APIs documented.
- Keep runtime exception messages identical across backends.
- Keep `CLAUDE.md` files limited to project preferences and external setup
  requirements. Do not duplicate facts derivable from source code or add bug
  histories or implementation diaries. Delete guides with nothing left to say.
- Before committing, run formatting, Clippy, and tests appropriate to the
  changed components. Use `cargo nextest run`, never plain `cargo test`.

For workspace-wide validation:

```bash
cargo fmt --check
cargo nextest run --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
