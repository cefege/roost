// Per-tool presentation helpers. Derive a one-line summary (always shown, so a
// glance tells you WHAT each tool did) and the primary argument payload worth
// rendering as code (write content, edit ops, bash command). Kept pure + here
// so ToolCard stays declarative and the harness can assert summaries directly.

export function parseArgs(argsJson: string): Record<string, unknown> {
  if (!argsJson) return {};
  try {
    const v: unknown = JSON.parse(argsJson);
    return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

const str = (v: unknown): string => (typeof v === "string" ? v : "");
const firstLine = (s: string): string => s.split("\n", 1)[0] ?? "";

/** One-line "what did this tool do" summary from its args. */
export function toolSummary(name: string, args: Record<string, unknown>): string {
  switch (name) {
    case "read": case "write": case "lsp":
      return str(args.path) || str(args.file);
    case "edit": {
      const p = str(args.path);
      if (p) return p;
      // Roost edit-language: path lives in the "[path#tag]" header of `input`.
      return firstLine(str(args.input)).replace(/^\[|[#\]].*$/g, "");
    }
    case "bash":
      return firstLine(str(args.command)).slice(0, 160);
    case "grep":
      return [str(args.pattern), str(args.path)].filter(Boolean).join("  ·  ");
    case "glob":
      return str(args.path) || str(args.pattern);
    case "todo":
      return str(args.op);
    case "web_search":
      return str(args.query);
    default:
      return str(args.i) || str(args.path) || str(args.query) || "";
  }
}

/** The argument that IS the substance (shown as a code block above the result).
 *  Null when the result text carries everything (read/grep/glob/todo/…). */
export function toolPayload(name: string, args: Record<string, unknown>): { lang: string; text: string } | null {
  switch (name) {
    case "write": {
      const t = str(args.content);
      return t ? { lang: langForPath(str(args.path)), text: t } : null;
    }
    case "edit": {
      const t = str(args.input);
      return t ? { lang: "diff", text: t } : null;
    }
    case "bash": {
      const t = str(args.command);
      return t ? { lang: "bash", text: t } : null;
    }
    default:
      return null;
  }
}

function langForPath(path: string): string {
  const ext = path.slice(path.lastIndexOf(".") + 1).toLowerCase();
  const byExt: Record<string, string> = {
    ts: "ts", tsx: "tsx", js: "js", jsx: "jsx", json: "json", md: "md",
    css: "css", html: "html", py: "py", rs: "rs", go: "go", sh: "bash", toml: "toml", yaml: "yaml", yml: "yaml",
  };
  return byExt[ext] ?? "text";
}
