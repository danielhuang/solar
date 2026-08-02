// The compiler's LSP server supplies semantic highlighting. The bundled
// TextMate grammar provides lexical fallback highlighting.

const path = require("path");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;

function activate(context) {
  const command = process.env.SOLAR_LSP_PATH || path.join(
    context.extensionPath,
    "server",
    process.platform === "win32" ? "lsp.exe" : "lsp",
  );
  client = new LanguageClient(
    "solar",
    "Solar language server",
    { command, transport: TransportKind.stdio },
    { documentSelector: [{ language: "solar" }] },
  );
  context.subscriptions.push(client.start());
}

function deactivate() {
  return client?.stop();
}

module.exports = { activate, deactivate };
