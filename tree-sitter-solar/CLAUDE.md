# Grammar

The grammar source is `grammar.js`. `src/parser.c` and the other files under
`tree-sitter-solar/src` are generated; do not edit them by hand.

When changing syntax:

1. Edit `grammar.js`.
2. Update CST-to-AST conversion in `../src/parser.rs`.
3. Update or add parser, type-check, and runtime fixtures as appropriate.
4. Update `../examples/example.solar` when the canonical example uses the
   changed syntax.
5. Run `cargo build` and the relevant tests.

`tree-sitter-solar/build.rs` runs `tree-sitter generate` when the grammar
changes.
