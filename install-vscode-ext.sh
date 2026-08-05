#!/usr/bin/env bash

set -euo pipefail

repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
extension_dir="$repo_dir/vscode-ext"
vsix_path="$extension_dir/solar-lang-latest.vsix"
vscode_cli="${VSCODE_CLI:-code}"

if ! command -v "$vscode_cli" >/dev/null 2>&1; then
    echo "VS Code CLI not found: $vscode_cli" >&2
    echo "Set VSCODE_CLI to the command or path for your VS Code installation." >&2
    exit 1
fi

cd "$extension_dir"
npm ci
npm run build:lsp
npx --yes @vscode/vsce package --allow-missing-repository --out "$vsix_path"
"$vscode_cli" --install-extension "$vsix_path" --force
