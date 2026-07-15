// Tree-sitter powered syntax highlighting for Solar, delivered as VS Code
// semantic tokens. We parse each document with the WASM build of the Solar
// grammar (built from ../tree-sitter-solar), run the `queries/highlights.scm`
// query, and translate the capture names into semantic tokens that the active
// color theme paints. There is no TextMate grammar — highlighting comes purely
// from tree-sitter.

const vscode = require("vscode");
const path = require("path");
const fs = require("fs");
const { Parser, Language, Query } = require("web-tree-sitter");

// ── Semantic token legend ────────────────────────────────────────────────────
// The order here defines the numeric indices used when encoding tokens.
const TOKEN_TYPES = [
  "comment",
  "string",
  "number",
  "keyword",
  "operator",
  "function",
  "method",
  "type",
  "typeParameter",
  "enumMember",
  "property",
  "parameter",
  "variable",
  "namespace",
  "decorator",
];
const TOKEN_MODIFIERS = ["readonly", "defaultLibrary"];

const legend = new vscode.SemanticTokensLegend(TOKEN_TYPES, TOKEN_MODIFIERS);

const typeIndex = new Map(TOKEN_TYPES.map((t, i) => [t, i]));
const modBit = new Map(TOKEN_MODIFIERS.map((m, i) => [m, 1 << i]));

// Map a tree-sitter capture name to a [tokenType, modifiers[]] pair. Anything
// not listed (e.g. `punctuation.*`) is left uncolored — default foreground.
const CAPTURE_MAP = {
  "comment": ["comment"],
  "string": ["string"],
  "number": ["number"],
  "boolean": ["keyword"],
  "constant.builtin": ["keyword"],
  "constant": ["variable", ["readonly"]],
  "keyword": ["keyword"],
  "function": ["function"],
  "function.call": ["function"],
  "function.method": ["method"],
  "function.method.call": ["method"],
  "type": ["type"],
  "type.builtin": ["type", ["defaultLibrary"]],
  "type.parameter": ["typeParameter"],
  "constructor": ["enumMember"],
  "variable": ["variable"],
  "variable.member": ["property"],
  "variable.parameter": ["parameter"],
  "module": ["namespace"],
  "attribute": ["decorator"],
  "operator": ["operator"],
};

// Resolve a capture name to legend indices, honoring dotted fallbacks:
// `keyword.import` -> `keyword`, `number.float` -> `number`, etc.
const resolvedCache = new Map();
function resolveCapture(name) {
  if (resolvedCache.has(name)) return resolvedCache.get(name);
  let key = name;
  let entry = null;
  while (key) {
    if (CAPTURE_MAP[key]) {
      entry = CAPTURE_MAP[key];
      break;
    }
    const dot = key.lastIndexOf(".");
    if (dot === -1) break;
    key = key.slice(0, dot);
  }
  let result = null;
  if (entry) {
    const ti = typeIndex.get(entry[0]);
    if (ti !== undefined) {
      let mods = 0;
      for (const m of entry[1] || []) mods |= modBit.get(m) || 0;
      result = { type: ti, mods };
    }
  }
  resolvedCache.set(name, result);
  return result;
}

// Convert a byte column within a line to a UTF-16 character column. VS Code
// positions are UTF-16 based; tree-sitter columns are byte offsets. For pure
// ASCII lines (the common case) they coincide, so we fast-path that.
function byteToChar(lineText, byteCol) {
  if (byteCol === 0) return 0;
  if (Buffer.byteLength(lineText, "utf8") === lineText.length) return byteCol;
  return Buffer.from(lineText, "utf8").slice(0, byteCol).toString("utf8").length;
}

// ── Lazy async initialization of the parser / query ──────────────────────────
let initPromise = null;
let parser = null;
let query = null;

function init(context) {
  if (initPromise) return initPromise;
  initPromise = (async () => {
    const runtimeWasm = require.resolve("web-tree-sitter/tree-sitter.wasm");
    await Parser.init({ locateFile: () => runtimeWasm });
    const grammarWasm = context.asAbsolutePath(
      path.join("syntaxes", "tree-sitter-solar.wasm"),
    );
    const lang = await Language.load(grammarWasm);
    parser = new Parser();
    parser.setLanguage(lang);
    const scm = fs.readFileSync(
      context.asAbsolutePath(path.join("queries", "highlights.scm")),
      "utf8",
    );
    query = new Query(lang, scm);
  })();
  return initPromise;
}

// ── Semantic tokens provider ─────────────────────────────────────────────────
const provider = (context) => ({
  async provideDocumentSemanticTokens(document) {
    await init(context);
    const builder = new vscode.SemanticTokensBuilder(legend);
    const text = document.getText();
    const tree = parser.parse(text);
    try {
      const captures = query.captures(tree.rootNode);

      // First-match-wins per start position: keep the capture with the lowest
      // pattern index (patterns earlier in highlights.scm win), matching the
      // query's authored precedence.
      const byStart = new Map();
      for (const cap of captures) {
        const start = cap.node.startIndex;
        const prev = byStart.get(start);
        if (!prev || cap.patternIndex < prev.patternIndex) byStart.set(start, cap);
      }
      const chosen = [...byStart.values()].sort(
        (a, b) => a.node.startIndex - b.node.startIndex,
      );

      let lastEnd = -1;
      for (const cap of chosen) {
        const node = cap.node;
        if (node.startIndex < lastEnd) continue; // skip overlaps
        const mapped = resolveCapture(cap.name);
        if (!mapped) continue;
        const start = node.startPosition;
        const end = node.endPosition;
        if (start.row !== end.row) continue; // no multi-line tokens in Solar
        const lineText = document.lineAt(start.row).text;
        const startChar = byteToChar(lineText, start.column);
        const endChar = byteToChar(lineText, end.column);
        const length = endChar - startChar;
        if (length <= 0) continue;
        builder.push(start.row, startChar, length, mapped.type, mapped.mods);
        lastEnd = node.endIndex;
      }
    } finally {
      tree.delete();
    }
    return builder.build();
  },
});

function activate(context) {
  context.subscriptions.push(
    vscode.languages.registerDocumentSemanticTokensProvider(
      { language: "solar" },
      provider(context),
      legend,
    ),
  );
}

function deactivate() {}

module.exports = { activate, deactivate };
