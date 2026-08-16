// syntaxLite.ts — minimal regex-based syntax highlighter.
// Returns an array of {text, kind} tokens from a single source line.
// kind maps to --syntax-<kind> CSS vars (keyword/string/comment/number/plain).
// JavaScript-ish rules applied for most extensions; good enough for code nav.
// Callers: FileViewerSheet.tsx per-line highlighting.
import { workerPathBasename } from "./nativePath.ts";

type TokenKind = "keyword" | "string" | "comment" | "number" | "plain";
export type Token = { text: string; kind: TokenKind };

// Keywords covering JS/TS/Go/Python/Rust/shell overlap.
const KEYWORDS = new Set([
  "break", "case", "catch", "class", "const", "continue", "debugger",
  "default", "delete", "do", "else", "export", "extends", "false",
  "finally", "for", "from", "function", "if", "import", "in",
  "instanceof", "interface", "let", "new", "null", "of", "return",
  "static", "super", "switch", "this", "throw", "true", "try", "type",
  "typeof", "undefined", "var", "void", "while", "with", "yield",
  "async", "await", "enum", "implements", "package", "private",
  "protected", "public", "readonly", "abstract", "declare", "namespace",
  // Go
  "func", "go", "defer", "chan", "select", "map", "range", "make", "len",
  "cap", "append", "copy", "close", "delete", "print", "println",
  // Python
  "def", "lambda", "pass", "not", "and", "or", "is", "as", "with",
  "global", "nonlocal", "assert", "raise", "except", "elif",
  // Rust
  "fn", "let", "mut", "impl", "trait", "struct", "where", "mod", "use",
  "pub", "self", "crate", "super", "move", "unsafe", "extern",
  "ref", "match", "loop", "dyn",
]);

// Token-level scanner — processes one line at a time.
// Does NOT handle multi-line strings/comments; suitable for line-by-line display.
function tokenizeLine(line: string, inBlockComment: boolean): {
  tokens: Token[];
  endsInBlockComment: boolean;
} {
  const tokens: Token[] = [];
  let i = 0;
  let stillInBlock = inBlockComment;

  function push(text: string, kind: TokenKind) {
    if (text.length > 0) tokens.push({ text, kind });
  }

  // If we enter this line mid-block-comment, consume until */ or end.
  if (stillInBlock) {
    const close = line.indexOf("*/", i);
    if (close === -1) {
      push(line, "comment");
      return { tokens, endsInBlockComment: true };
    }
    push(line.slice(0, close + 2), "comment");
    i = close + 2;
    stillInBlock = false;
  }

  while (i < line.length) {
    // Block comment start.
    if (line[i] === "/" && line[i + 1] === "*") {
      const close = line.indexOf("*/", i + 2);
      if (close === -1) {
        push(line.slice(i), "comment");
        return { tokens, endsInBlockComment: true };
      }
      push(line.slice(i, close + 2), "comment");
      i = close + 2;
      continue;
    }

    // Line comment (//).
    if (line[i] === "/" && line[i + 1] === "/") {
      push(line.slice(i), "comment");
      return { tokens, endsInBlockComment: false };
    }

    // Python/shell line comment (#).
    if (line[i] === "#") {
      push(line.slice(i), "comment");
      return { tokens, endsInBlockComment: false };
    }

    // String literals — single or double quoted, no escape handling beyond \\.
    if (line[i] === '"' || line[i] === "'" || line[i] === "`") {
      const q = line[i]!;
      let j = i + 1;
      while (j < line.length) {
        if (line[j] === "\\" && j + 1 < line.length) { j += 2; continue; }
        if (line[j] === q) { j++; break; }
        j++;
      }
      push(line.slice(i, j), "string");
      i = j;
      continue;
    }

    // Numbers (int, float, hex, binary, underscored).
    if (/[0-9]/.test(line[i]!)) {
      let j = i;
      while (j < line.length && /[0-9a-fA-FxXbBoO_.eE+\-]/.test(line[j]!)) j++;
      push(line.slice(i, j), "number");
      i = j;
      continue;
    }

    // Identifiers — check against keyword set.
    if (/[a-zA-Z_$]/.test(line[i]!)) {
      let j = i;
      while (j < line.length && /[a-zA-Z0-9_$]/.test(line[j]!)) j++;
      const word = line.slice(i, j);
      push(word, KEYWORDS.has(word) ? "keyword" : "plain");
      i = j;
      continue;
    }

    // Everything else — accumulate runs of punctuation/whitespace as plain.
    let j = i + 1;
    while (
      j < line.length &&
      !/[a-zA-Z0-9_$"'`#]/.test(line[j]!) &&
      !(line[j] === "/" && (line[j + 1] === "/" || line[j + 1] === "*"))
    ) {
      j++;
    }
    push(line.slice(i, j), "plain");
    i = j;
  }

  return { tokens, endsInBlockComment: false };
}

// Tokenize all lines of a file. Returns per-line token arrays.
// Tracks block-comment state across lines.
export function tokenizeLines(lines: string[]): Token[][] {
  const result: Token[][] = [];
  let inBlock = false;
  for (const line of lines) {
    const { tokens, endsInBlockComment } = tokenizeLine(line, inBlock);
    inBlock = endsInBlockComment;
    result.push(tokens);
  }
  return result;
}

// Extension → "should highlight" decision.
// Returns false for extensions where JS-ish rules produce noise (markdown, json, yaml).
export function shouldHighlight(ext: string): boolean {
  return new Set([
    "ts", "tsx", "js", "jsx", "mjs", "cjs",
    "go", "py", "rs", "c", "cpp", "h", "hpp",
    "java", "kt", "swift", "rb", "php", "sh", "bash", "zsh",
    "css", "scss", "less",
  ]).has(ext.toLowerCase());
}

// Extract extension from a file path (no dot, lowercase).
export function extFromPath(path: string, workerFp = ""): string {
  const base = workerPathBasename(workerFp, path);
  const dot = base.lastIndexOf(".");
  return dot >= 0 ? base.slice(dot + 1).toLowerCase() : "";
}
