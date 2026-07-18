// parseFileHref must invert CellTerminal.resolveFile's `/file/<fp>/<enc path>`
// format exactly — a mismatch means Cmd-clicking a terminal path downloads the
// wrong file (or nothing). Mirrors resolveFile's encoding here so the two can't
// drift silently.

import { describe, test, expect } from "bun:test";
import { parseFileHref } from "../src/lib/downloadWorkerFile.ts";

// Same transform CellTerminal.resolveFile applies to build the href.
function buildHref(workerFp: string, abs: string, line?: number): string {
  const enc = abs.split("/").map((s) => (s ? encodeURIComponent(s) : s)).join("/");
  return `/file/${workerFp}/${enc.replace(/^\//, "")}${line ? `#L${line}` : ""}`;
}

describe("parseFileHref inverts resolveFile", () => {
  test("plain absolute path", () => {
    expect(parseFileHref(buildHref("abc123", "/Users/you/x.ts"))).toEqual({
      workerFp: "abc123", path: "/Users/you/x.ts",
    });
  });

  test("strips the #L<line> fragment", () => {
    expect(parseFileHref(buildHref("abc123", "/repo/apps/web/foo.ts", 42))).toEqual({
      workerFp: "abc123", path: "/repo/apps/web/foo.ts",
    });
  });

  test("round-trips segments needing URL encoding (spaces, #, %)", () => {
    const abs = "/Users/me/my docs/a#b%c.ts";
    expect(parseFileHref(buildHref("fp", abs))).toEqual({ workerFp: "fp", path: abs });
  });

  test("non-file href → null (not a terminal file link)", () => {
    expect(parseFileHref("/s/some-session-id")).toBeNull();
    expect(parseFileHref("https://example.com/x")).toBeNull();
  });
});
