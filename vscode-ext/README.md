# Solar Language (VS Code)

Tree-sitter based syntax highlighting for the Solar programming language.

Unlike a typical VS Code language extension, highlighting here is **not** driven
by a TextMate grammar. The extension bundles a WebAssembly build of the Solar
tree-sitter grammar (`syntaxes/tree-sitter-solar.wasm`, built from
`../tree-sitter-solar`), parses each `.solar` document with
[`web-tree-sitter`](https://www.npmjs.com/package/web-tree-sitter), runs the
highlight query, and emits the results as VS Code **semantic tokens** that the
active color theme paints.

The highlight query lives at `../tree-sitter-solar/queries/highlights.scm` (the
single source of truth); `queries/highlights.scm` here is a generated copy of it
(git-ignored), refreshed by `npm run build:grammar`.

## Features

- Syntax highlighting only (no language server, no formatting).
- Powered by the same tree-sitter grammar + highlight query the compiler repo
  ships, so highlighting stays in sync with the real parser.
- Line-comment toggling and bracket matching / auto-closing.

## Regenerating the grammar WASM

```bash
npm run build:grammar
```

(Requires emscripten for the WASM build. This also copies the highlight query
from `../tree-sitter-solar/queries/highlights.scm`.)
