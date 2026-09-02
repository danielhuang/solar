# Solar Language (VS Code)

Semantic syntax highlighting for the Solar programming language.

Resolved syntax highlighting is provided by the Solar language server as VS
Code **semantic tokens**. A small TextMate grammar provides lexical fallback
highlighting.

## Features

- Compiler diagnostics while editing; each open file is checked as a program root.
- Autocomplete, semantic syntax highlighting, hover documentation, and
  go-to-definition.
- Powered by `solar-lsp`, using the same tree-sitter grammar as the compiler.
- Line-comment toggling and bracket matching / auto-closing.

## Building the language server

```bash
npm run build:lsp
```

The extension bundles and starts `server/lsp` by default. Set `SOLAR_LSP_PATH`
to use a different server executable during development.
