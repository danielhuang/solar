# Solar Language (VS Code)

Semantic syntax highlighting for the Solar programming language.

Highlighting is provided by the Solar language server as VS Code **semantic
tokens**. The server uses the compiler's tree-sitter grammar directly; this
extension does not bundle a TextMate grammar, WASM grammar, or `highlights.scm`
query.

## Features

- Semantic syntax highlighting only (no formatting or navigation).
- Powered by `solar-lsp`, using the same tree-sitter grammar as the compiler.
- Line-comment toggling and bracket matching / auto-closing.

## Building the language server

```bash
npm run build:lsp
```

The extension bundles and starts `server/lsp` by default. Set `SOLAR_LSP_PATH`
to use a different server executable during development.
