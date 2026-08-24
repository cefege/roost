#!/usr/bin/env bun
// scripts/lint-roost.ts — mechanical enforcement of the standing repo
// invariants: the recurring-failure guards (docs/FAILURE-INDEX.md), the
// design-system raw-value ratchet, the ≤400-line file cap, and the
// log-facade rule.
//
// Exit 0 = clean. Exit 1 = at least one violation (printed with file:line +
// memory pointer). Add a new check by appending to CHECKS below.
//
// Run: bun run lint           (blocking `lint` step of the ci.yml invariants job)
// Re-snapshot a ratchet: --update-design-baseline | --update-size-baseline
//                        --update-console-baseline

import { readdirSync, readFileSync, statSync } from "node:fs";
import { collectCounts, runRatchet, walk, type RatchetSpec } from "./lint-ratchet.ts";
import { join } from "node:path";

const REPO = new URL("..", import.meta.url).pathname;

interface Violation {
  file: string;
  line: number;
  text: string;
  rule: string;
  memory: string;
}

interface Check {
  rule: string;
  memory: string;
  files: RegExp;
  pattern: RegExp;
  // optional: gate via additional context lines around match
  ok?: (file: string, lineIdx: number, lines: string[]) => boolean;
}

// ───────────────────────────────────────────────────────────────────────
// CHECKS — one per CLAUDE.md L11 row, narrowest grep that catches the bad
// pattern without false positives.
// ───────────────────────────────────────────────────────────────────────

const CHECKS: Check[] = [
  {
    // M3 selected-state is ONE role: --md-sys-color-secondary-container (+ its
    // on-* text). The old per-theme --bg-selected/--border-selected canonical
    // tokens were random tints (green/teal) that read non-M3 on every theme;
    // they're removed. Any reintroduction in a component is the regression that
    // forced repeated micro-corrections. Selected states route through the M3
    // role only. theme-vars.css keeps back-compat aliases (styles/, not matched).
    rule: "M3: selected state must use --md-sys-color-secondary-container, not the removed --bg-selected/--border-selected tints",
    memory: "CLAUDE.md — design system",
    files: /apps\/web\/src\/.*\.tsx$/,
    pattern: /var\(--(bg|border)-selected\)/,
  },
  {
    rule: "L11: Solid setStore(key, fn → newRecord) on a Record silently no-ops",
    memory: "docs/FAILURE-INDEX.md",
    // Any apps/web/src file that imports setRootStore can ship the
    // pattern. Originally scoped to sync/projector but the bug also
    // hit pair-refresh in a component and an ad-hoc fix in MainPane —
    // scan all of apps/web/src.
    files: /apps\/web\/src\/.*\.(ts|tsx)$/,
    pattern: /setRootStore\(\s*["'][a-z_]+["']\s*,\s*\([^)]*\)\s*=>\s*\{/,
  },
  {
    rule: "L11: CellTerminal must render inside <For> deck, never <Show> (remount on nav loses scrollback)",
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/web\/src\/components\/MainPane\.tsx$/,
    // <Show ...>{(session) => <CellTerminal …  (Terminal.tsx deleted in the
    // cell-shipping cutover; the deck now hosts CellTerminal).
    pattern: /<Show[^>]*>\s*\{[^}]*=>\s*<CellTerminal\b/,
  },
  {
    rule: "L11: never read props.* inside an onCleanup callback",
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/web\/src\/.*\.tsx$/,
    // crude: onCleanup(...)... props.foo within the same function body
    pattern: /onCleanup\s*\(\s*\(?\)?\s*=>\s*\{[^}]*props\./,
  },
  {
    rule: "L11: sidebar data-selected must be URL-driven, never sessions().length",
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/web\/src\/components\/sidebar\/.*\.tsx$/,
    pattern: /data-selected\s*=\s*\{\s*sessions\(\)\.length/,
  },
  {
    rule: "L11: addToast kind must be 'ok' | 'warn' | 'err' (no 'info', 'success' etc)",
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/web\/src\/.*\.tsx?$/,
    pattern: /addToast\([^,)]+,\s*"(?!ok"|warn"|err")[a-z]+"/,
  },
  {
    // REGRESSED 3 TIMES. Synchronous wterm._doRender() inside the
    // byte chunk handler corrupts TUI rendering — claude / vim emit
    // ANSI sequences that span multiple WS frames (cursor save → draw
    // → cursor restore), and force-rendering between chunks paints
    // half-applied state. Trust wterm's built-in setTimeout(0)+rAF
    // coalescing. The smoke-harness "hidden tab rAF throttle" case
    // gets fixed in the harness (longer poll), NOT here.
    //
    // History: 0c4a7bca added it; reverted 7e817192 / 91YYY. User
    // reports 3+ regressions total.
    rule: "L11: never force _doRender() inside the CellTerminal byte handler",
    memory: "docs/FAILURE-INDEX.md",
    // Terminal.tsx deleted in the cell-shipping cutover; the byte handler
    // (now feeding the hidden input/mode-oracle wterm) lives in CellTerminal.
    files: /apps\/web\/src\/components\/CellTerminal\.tsx$/,
    ok: (_file, _i, lines) => {
      const txt = lines.join("\n");
      // Find the registerBytesHandler closure body. Any _doRender
      // reference inside it (call OR alias to _doRender) is the bug.
      const m = txt.match(/registerBytesHandler\([\s\S]*?\}\s*\)\s*;/);
      if (!m) return true;
      // Strip line + block comments before testing so we don't flag
      // the historical-context comment that names _doRender.
      const stripped = m[0]
        .replace(/\/\*[\s\S]*?\*\//g, "")
        .replace(/\/\/.*$/gm, "");
      // Bug pattern: a _doRender CALL — plain `_doRender(` OR optional-chained
      // `_doRender?.(`. The dot is optional (a prior regex required it and so
      // only ever caught the `?.` form, missing the plain call it claimed to).
      return !/_doRender\s*\??\.?\s*\(/.test(stripped);
    },
  },
  {
    // A live SCD resize must update the existing terminal core at the keeper's
    // ordered boundary. Reintroducing the old rebuild/claim path makes static
    // cells and scrollback continuity depend on timing again.
    //
    // Absence is a violation: either the boundary moved and this guard must be
    // retargeted, or the in-place resize was dropped.
    rule: "L11: worker stream resize must update the existing terminal core",
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/worker\/src\/session-resize-capture\.ts$/,
    ok: (_file, _i, lines) => {
      const txt = lines.join("\n");
      return /rec\.wtermCore\.resize\(capture\.toCols,\s*capture\.toRows\)/.test(txt);
    },
  },
  {
    // att1f — attachment flow must go through PTY round-trip.
    // wterm.write* paints into the local buffer without traversing the
    // worker, so claude / the underlying tool never SEES the typed path.
    // The user would see the path on screen but tools couldn't read it.
    rule: "att1: attachment code must NOT call wterm.write* — paint via PTY",
    memory: "docs/archive/phase-att1.md",
    files: /apps\/web\/src\/lib\/attachments\.ts$/,
    pattern: /wterm\.(write|writeRaw|writeString)\(/,
  },

  // ─── phase-24e: single-transport invariants ────────────────────────
  // The rules below mechanically prevent the "two transports per seam"
  // shape that phase-24 collapses from coming back. Each carries a
  // narrow allowlist of files where the construct IS legitimate —
  // grow that list ONLY if a new transport surface is intentionally
  // added.
  // ─── phase-24e: single-transport invariants ────────────────────────
  // Each rule below uses an `ok()` that allowlists the canonical
  // transport file AND short-circuits if the bad pattern is absent
  // from the file. Allowlists shrink as phase-24c/d delete files.
  {
    rule: "phase-24: `new WebSocket(` outside the canonical client/server links",
    memory: "docs/archive/phase-24.md",
    files: /apps\/(web|worker)\/src\/.*\.(ts|tsx)$/,
    ok: (file, _i, lines) => {
      const ALLOW = [
        // trpc.ts deleted in crpc6. deepgramDictation dials Deepgram's live
        // STT endpoint — a genuine external WS, not an intra-Roost transport.
        "apps/web/src/lib/deepgramDictation.ts",
        "apps/worker/src/transport/coord-link.ts",
        // sync.ts holds the canonical web↔coord Sync-stream client (the only
        // web-side sync WebSocket; the SPA analog of CoordLink on the worker).
        "apps/web/src/store/sync.ts",
      ];
      if (ALLOW.some((s) => file.endsWith(s))) return true;
      return !/new\s+WebSocket\s*\(/.test(lines.join("\n"));
    },
  },
  {
    rule: "phase-24: `Bun.serve({ websocket })` outside coord main",
    memory: "docs/archive/phase-24.md",
    files: /apps\/(coord|worker)\/src\/.*\.ts$/,
    ok: (file, _i, lines) => {
      const ALLOW = [
        "apps/coord/src/main.ts",
      ];
      if (ALLOW.some((s) => file.endsWith(s))) return true;
      return !/Bun\.serve\s*\(\s*\{[\s\S]*?\bwebsocket\s*:/.test(lines.join("\n"));
    },
  },
  {
    rule: "phase-24: module-level `let _ws` / `let _reconnectTimer` in apps/web/src/store",
    memory: "docs/archive/phase-24.md",
    files: /apps\/web\/src\/store\/.*\.ts$/,
    ok: (_file, _i, lines) => {
      // events-ws.ts deleted in 24c-3; deny-all now.
      return !/^\s*let\s+_(?:ws|reconnectTimer|backoffMs)\b/m.test(lines.join("\n"));
    },
  },

  // ─── NOW-tranche: previously-ungated L11 rows (regression-loop closure) ──
  // Each row below was in CLAUDE.md L11 but had NO mechanical guard, so it
  // could silently re-ship. The matrix that found them lives in the session
  // analysis; these four close the cheapest (one-check) gaps.
  {
    // The bug: `JSON.parse(row.X)` placed directly inside a *Bus.publish({…})
    // payload. A partial/hand-edited row throws SyntaxError AFTER the mutation
    // committed → the RPC 500s → the bus subscriber never fires → SPA shows
    // stale state until refresh (split-brain). Request-time JSON.parse(req.X)
    // with a try/reject is the CORRECT sibling pattern and is NOT flagged —
    // only raw parse lexically inside a publish() call is. Fix: build the
    // value with safeJsonParse BEFORE publish, publish the variable.
    rule: "L11: raw JSON.parse() inside a *Bus.publish() payload — parse-after-commit 500s the RPC → split-brain; use safeJsonParse",
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/coord\/src\/connect\/handlers-.*\.ts$/,
    ok: (_file, _i, lines) => {
      // Walk each `.publish(` call from its open-paren to the matching close,
      // capturing ONLY that call's span (no cross-function false positives a
      // whole-file regex would hit). Flag JSON.parse inside the span.
      for (let i = 0; i < lines.length; i++) {
        const idx = lines[i]!.indexOf(".publish(");
        if (idx < 0) continue;
        let depth = 0;
        let chunk = "";
        for (let j = i; j < lines.length && j < i + 20; j++) {
          const seg = j === i ? lines[j]!.slice(idx + 8) : lines[j]!; // +8 → start at "("
          chunk += seg + "\n";
          for (const ch of seg) {
            if (ch === "(") depth++;
            else if (ch === ")") depth--;
          }
          if (chunk.includes("(") && depth <= 0) break;
        }
        if (/JSON\.parse\b/.test(chunk)) return false;
      }
      return true;
    },
  },
  {
    // The bug: writeAuditLog called from the outer coord-factory fetch wrapper
    // instead of the AuthInterceptor → caller_fp=NULL on every authed RPC (the
    // interceptor sets caller on per-RPC contextValues the outer wrapper can't
    // see). The audit row must be written INSIDE the interceptor's try/finally
    // where the verified caller is in scope.
    rule: "L11: writeAuditLog must be CALLED inside the AuthInterceptor (else audit_log caller_fp=NULL)",
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/coord\/src\/connect\/auth-interceptor\.ts$/,
    ok: (_file, _i, lines) => lines.join("\n").includes("writeAuditLog("),
  },
  {
    // The bug: Bun.spawn({terminal:{…}}) does NOT inject TERM into the spawned
    // child's env (node-pty did). Locally-bootstrapped workers inherit TERM
    // from Terminal.app and hide it; SSH-bootstrapped (deployed) workers see
    // TERM="" / "unknown" → backspace=space, ncurses "$TERM=unknown". Tests
    // that pass TERM:… in themselves false-cover it — this pins the explicit
    // assignment at the real keeper spawn site.
    rule: "L11: keeper Bun.spawn env must set TERM explicitly (deployed-only ncurses $TERM=unknown)",
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/worker\/src\/keeper\/keeper-frame-handler\.ts$/,
    ok: (_file, _i, lines) => /TERM:\s*["']xterm/.test(lines.join("\n")),
  },
  {
    // The bug: wterm renders scrollback as .term-scrollback-row DOM elements;
    // without `.wterm { overflow-y: auto }` the rows are clipped and the user
    // cannot scroll up to see history ("THERE IS NO SCROLL"). The fix is this
    // one CSS rule — NOT switching terminal cores. Pin it to the .wterm block.
    rule: "L11: .wterm must keep overflow-y: auto (scrollback rows clip otherwise — do NOT switch cores)",
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/web\/src\/styles\/sidebar\.css$/,
    ok: (_file, _i, lines) =>
      /\.wterm\s*\{[^}]*overflow-y\s*:\s*auto[^}]*\}/.test(lines.join("\n")),
  },
  {
    // node:zlib (sync brotli/gzip OR createGzip stream) corrupts the heap under
    // Bun → ~11h-MTBF corrupted-pointer segfault that takes the whole always-on
    // coord down (15 crashes; commit 1f75e4ae). Bun.gzipSync (native zlib-ng) is
    // the crash-safe path. connect-node compression was the first vector, the
    // main.ts static-asset + backup.ts buffer/stream calls were the second.
    rule: "coord must NOT import node:zlib (heap-corruption segfault under Bun — use Bun.gzipSync)",
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/coord\/src\/.*\.ts$/,
    pattern: /from\s+["']node:zlib["']|require\(\s*["']node:zlib["']\s*\)/,
  },
];

// ───────────────────────────────────────────────────────────────────────

function runPatternChecks(): Violation[] {
  const out: Violation[] = [];
  for (const file of walk(REPO)) {
    const rel = file.slice(REPO.length).replace(/^\/+/, "");
    let text: string;
    try { text = readFileSync(file, "utf8"); } catch { continue; }
    const lines = text.split("\n");
    for (const c of CHECKS) {
      if (!c.files.test(rel)) continue;
      if (c.ok) {
        if (!c.ok(file, 0, lines)) {
          out.push({ file: rel, line: 1, text: "<missing required pattern>", rule: c.rule, memory: c.memory });
        }
        continue;
      }
      lines.forEach((line, i) => {
        if (c.pattern.test(line)) {
          out.push({ file: rel, line: i + 1, text: line.trim().slice(0, 140), rule: c.rule, memory: c.memory });
        }
      });
    }
  }
  return out;
}

// ───────────────────────────────────────────────────────────────────────
// Color-fallback check: parse theme-vars.css for declared --names, then
// scan apps/web/src/**/*.{tsx,ts,css} for var(--name, ...) and flag any
// reference whose --name is not declared.
// ───────────────────────────────────────────────────────────────────────

function runColorFallbackCheck(): Violation[] {
  const out: Violation[] = [];
  const themeFile = join(REPO, "apps/web/src/styles/theme-vars.css");
  let theme: string;
  try { theme = readFileSync(themeFile, "utf8"); }
  catch { return out; }
  const declared = new Set<string>(
    [...theme.matchAll(/--([a-z0-9-]+)\s*:/gi)].map((m) => m[1]!),
  );
  // Other CSS files also declare tokens (sidebar.css has --df-row-h
  // scoped to aside; syntax-vars.css has --syntax-* per theme block).
  // Any token referenced by var(...) must resolve in at least one of
  // these declaration files.
  for (const css of [
    "apps/web/src/styles/sidebar.css",
    "apps/web/src/styles/syntax-vars.css",
    // M3 shape/spacing/typography tokens (--md-space-*, --md-title-*, etc.)
    // live here, not in theme-vars.css.
    "apps/web/src/components/Settings/md/tokens.css",
    // Voice-input FAB motion tokens (--md-ease-*) are declared + used here.
    "apps/web/src/styles/voice-input.css",
  ]) {
    try {
      const txt = readFileSync(join(REPO, css), "utf8");
      for (const m of txt.matchAll(/--([a-z0-9-]+)\s*:/gi)) declared.add(m[1]!);
    } catch { /* ok */ }
  }

  for (const file of walk(join(REPO, "apps/web/src"))) {
    const rel = file.slice(REPO.length).replace(/^\/+/, "");
    if (!/\.(tsx|ts|css)$/.test(rel)) continue;
    if (rel.endsWith(".test.tsx") || rel.endsWith(".test.ts") || rel.includes("/e2e/")) continue;
    // The CSS declaration files themselves reference --name in commentary
    // and inside var(...) definitions — don't lint declarations against
    // themselves.
    if (
      rel.endsWith("/theme-vars.css") ||
      rel.endsWith("/sidebar.css") ||
      rel.endsWith("/syntax-vars.css")
    ) continue;
    let text: string;
    try { text = readFileSync(file, "utf8"); } catch { continue; }
    const lines = text.split("\n");
    lines.forEach((line, i) => {
      // Skip CSS comments (rough — sufficient for one-line comments).
      if (line.trim().startsWith("//") || line.trim().startsWith("*")) return;
      for (const m of line.matchAll(/var\(\s*--([a-z0-9-]+)\s*[,)]/g)) {
        const name = m[1]!;
        if (declared.has(name)) continue;
        // Common false-positives:
        if (name.startsWith("term-")) continue;        // wterm declares its own
        if (name.startsWith("df-")) continue;          // sidebar-scoped
        if (name === "font-anthropicons") continue;    // claude.ai/code paste
        out.push({
          file: rel,
          line: i + 1,
          text: line.trim().slice(0, 140),
          rule: `L11: var(--${name}) is not declared in theme-vars.css or sidebar.css`,
          memory: "docs/FAILURE-INDEX.md",
        });
      }
    });
  }
  return out;
}

// ───────────────────────────────────────────────────────────────────────
// Hardcoded-color-fallback check: ban var(--token, #hex) / var(--token,
// rgba(...)). Every color token is guaranteed declared by the theme engine
// (lib/theme.ts writes the canonical set; theme-vars.css aliases the rest),
// so a hardcoded color fallback is dead code AND a landmine — if the token
// ever went undefined it would silently paint the wrong color against the
// active theme. Non-color fallbacks (px, font names, var refs) are allowed.
// See docs/FAILURE-INDEX.md.
// ───────────────────────────────────────────────────────────────────────

function runHardcodedFallbackCheck(): Violation[] {
  const out: Violation[] = [];
  const COLOR_FALLBACK = /var\(\s*--[a-z0-9-]+\s*,\s*(#[0-9a-fA-F]{3,8}|rgba?\([^()]*\))\s*\)/g;
  for (const file of walk(join(REPO, "apps/web/src"))) {
    const rel = file.slice(REPO.length).replace(/^\/+/, "");
    if (!/\.(tsx|ts|css)$/.test(rel)) continue;
    if (rel.endsWith(".test.tsx") || rel.endsWith(".test.ts") || rel.includes("/e2e/")) continue;
    let text: string;
    try { text = readFileSync(file, "utf8"); } catch { continue; }
    text.split("\n").forEach((line, i) => {
      if (COLOR_FALLBACK.test(line)) {
        out.push({
          file: rel, line: i + 1, text: line.trim().slice(0, 140),
          rule: "L11: hardcoded color fallback var(--x, #hex) — tokens are always defined; drop the fallback",
          memory: "docs/FAILURE-INDEX.md",
        });
      }
      COLOR_FALLBACK.lastIndex = 0;
    });
  }
  return out;
}

// ───────────────────────────────────────────────────────────────────────
// Raw-value ratchet (design cohesion): ban NEW hardcoded hex / rgb() / px
// font-sizes in component files. Token-DEFINITION files (theme-vars, md/tokens,
// syntax-vars, voice-input) are exempt — they DECLARE the values everything
// else must reference via var(). A per-file baseline
// (scripts/design-raw-baseline.json) records the existing offender count so the
// 130+ legacy offenders don't fail the build; a file FAILS only when its count
// INCREASES (new drift). Migration lowers counts → re-baseline with
// `bun scripts/lint-roost.ts --update-design-baseline`. This is the load-bearing
// design-system enforcement: hardcoding a color/size in new UI stops compiling.
// ───────────────────────────────────────────────────────────────────────

const RAW_VALUE_ALLOW = new Set([
  "apps/web/src/styles/theme-vars.css",
  "apps/web/src/components/Settings/md/tokens.css",
  // icon.css DECLARES the .md-icon font-size utility (24/18/32px) that
  // .md-icon--sm/--lg reference — moved verbatim out of md/tokens.css, same
  // definition-file exemption.
  "apps/web/src/components/Settings/md/icon.css",
  "apps/web/src/styles/syntax-vars.css",
  "apps/web/src/styles/voice-input.css",
  // themes.ts DEFINES the theme palettes — raw hex is the source of the
  // canonical tokens here, not drift. Exempt (design-system phase 2 triage).
  "apps/web/src/lib/themes.ts",
  // agents.ts DECLARES the BUILTIN_AGENTS brand palette — these hexes are
  // agent identity data the palette defines, not theme drift (cf. icon.css).
  "apps/web/src/lib/agents.ts",
]);
const RAW_BASELINE_FILE = join(REPO, "scripts/design-raw-baseline.json");

// Count lines in one file carrying a raw hex color, rgb()/rgba(), or px
// font-size — excluding var(--x, …) fallbacks (owned by the fallback check)
// and comment lines.
function rawValueLineCount(text: string): number {
  let n = 0;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (line.startsWith("//") || line.startsWith("*") || line.startsWith("/*")) continue;
    // Strip var() fallbacks so their inner hex/rgb isn't double-counted here.
    const stripped = line.replace(/var\(\s*--[a-z0-9-]+\s*,[^)]*\)/g, "");
    if (
      /#[0-9a-fA-F]{3,8}\b/.test(stripped) ||
      /\brgba?\(/.test(stripped) ||
      /font-?size\s*:\s*['"]?\s*\d+px/i.test(line)
    ) n++;
  }
  return n;
}

const RAW_RATCHET: RatchetSpec = {
  baselineFile: RAW_BASELINE_FILE,
  updateFlag: "--update-design-baseline",
  freshAllowance: 0,
  text: (n, allowed) => `${n} raw hex/rgb/px-font value lines (baseline ${allowed}) — reference a theme token instead`,
  rule: "design: no NEW raw color/px-font values — use --md-*/--surface-*/--text-* + the type ramp (ratcheted)",
  memory: "CLAUDE.md — design system",
  unit: "raw-value lines",
};

function rawCounts(): Record<string, number> {
  return collectCounts([join(REPO, "apps/web/src")], (rel) =>
    /\.(tsx|ts|css)$/.test(rel)
    && !rel.endsWith(".test.tsx")
    && !rel.endsWith(".test.ts")
    && !rel.includes("/e2e/")
    && !RAW_VALUE_ALLOW.has(rel),
  rawValueLineCount);
}


// ───────────────────────────────────────────────────────────────────────
// File-size ratchet: a source file stays ≤400 lines. A per-file baseline
// (scripts/file-size-baseline.json) freezes the files that were already
// over the cap when the rule went live, so the cap is enforceable without
// one repo-wide split: a baselined file FAILS only when it grows PAST its
// recorded count, and a file ABSENT from the baseline may never exceed the
// cap at all. Splits lower counts → re-snapshot with
// `bun scripts/lint-roost.ts --update-size-baseline`. Generated protoc
// output (apps/shared/src/gen) is excluded — nobody hand-splits it.
// ───────────────────────────────────────────────────────────────────────

const FILE_LINE_CAP = 400;
const SIZE_BASELINE_FILE = join(REPO, "scripts/file-size-baseline.json");
const SIZE_EXCLUDE = /^apps\/[^/]+\/src\/gen\//;

// Hand-written source roots the cap governs: every app's src + tests, plus
// the two tool trees. Enumerated from the filesystem so a new app is covered
// the day it lands.
function sizeRoots(): string[] {
  const out: string[] = [];
  for (const app of readdirSync(join(REPO, "apps"), { withFileTypes: true })) {
    if (!app.isDirectory()) continue;
    for (const sub of ["src", "tests"]) out.push(join(REPO, "apps", app.name, sub));
  }
  for (const dir of ["scripts", "smoke"]) out.push(join(REPO, dir));
  return out.filter((p) => { try { return statSync(p).isDirectory(); } catch { return false; } });
}

const SIZE_RATCHET: RatchetSpec = {
  baselineFile: SIZE_BASELINE_FILE,
  updateFlag: "--update-size-baseline",
  freshAllowance: FILE_LINE_CAP,
  guardFloor: FILE_LINE_CAP + 1,
  text: (n, allowed) => `${n} lines (cap ${FILE_LINE_CAP}, baseline ${allowed}) — split before growing`,
  rule: "size: files stay ≤400 lines; baselined files may only shrink (ratcheted)",
  memory: "CLAUDE.md — coding standards",
  unit: "lines",
};

function fileSizes(): Record<string, number> {
  return collectCounts(sizeRoots(), (rel) => /\.(ts|tsx)$/.test(rel) && !SIZE_EXCLUDE.test(rel), (text) => text.split("\n").length);
}


// ───────────────────────────────────────────────────────────────────────
// Log-facade ratchet: coord and worker are long-lived services whose output
// is machine-read, so they log through `log` from @roost/shared/log — the one
// facade that stamps ev/level and owns the console sink (apps/shared/src/log.ts,
// outside the roots scanned here). A raw console.* in a service bypasses it.
// The surviving callsites are pre-logger bootstrap and fatal-exit paths; the
// baseline (scripts/console-baseline.json) freezes them so the rule blocks NEW
// drift. apps/roost-cli is deliberately out of scope — stdout is its product
// surface — as is apps/web, which routes through diag()/signal().
// Re-snapshot: `bun scripts/lint-roost.ts --update-console-baseline`.
// ───────────────────────────────────────────────────────────────────────

const CONSOLE_BASELINE_FILE = join(REPO, "scripts/console-baseline.json");
const CONSOLE_ROOTS = ["apps/coord/src", "apps/worker/src"];
const CONSOLE_CALL = /\bconsole\.(log|warn|error|info|debug)\s*\(/;

function consoleLineCount(text: string): number {
  let n = 0;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (line.startsWith("//") || line.startsWith("*") || line.startsWith("/*")) continue;
    if (CONSOLE_CALL.test(line)) n++;
  }
  return n;
}

const CONSOLE_RATCHET: RatchetSpec = {
  baselineFile: CONSOLE_BASELINE_FILE,
  updateFlag: "--update-console-baseline",
  freshAllowance: 0,
  text: (n, allowed) => `${n} console.* call lines (baseline ${allowed}) — log through the @roost/shared/log facade`,
  rule: "logging: use the log facade from @roost/shared/log, not console.* (ratcheted)",
  memory: "CLAUDE.md — coding standards",
  unit: "console.* lines",
};

function consoleCounts(): Record<string, number> {
  return collectCounts(CONSOLE_ROOTS.map((root) => join(REPO, root)), (rel) => /\.ts$/.test(rel), consoleLineCount);
}

// ───────────────────────────────────────────────────────────────────────
// Header-presence ratchet (CLAUDE.md coding standard #4): every
// hand-written app source file opens with a WHY header of ≥3 comment
// lines. Only PRESENCE is gated — length past the minimum stays style.
// Files already short when the rule landed are frozen in
// scripts/header-baseline.json keyed by their header-line count: fixing a
// file drops its entry at the next re-snapshot, and shrinking one further
// still fails. Generated output and tests are exempt; script-entry
// runners that violate are simply baselined like any legacy file.
// Re-snapshot: `bun scripts/lint-roost.ts --update-header-baseline`.
// ───────────────────────────────────────────────────────────────────────

const HEADER_MIN_LINES = 3;
const HEADER_BASELINE_FILE = join(REPO, "scripts/header-baseline.json");
const HEADER_EXCLUDE = [/^apps\/[^/]+\/src\/gen\//, /\.generated\.ts$/, /\.d\.ts$/];

// Length of the leading comment run: consecutive // lines plus one /*…*/
// block. Blank lines and a shebang before the run count neither way.
function leadingHeaderLines(text: string): number {
  const lines = text.split("\n");
  let i = lines[0]?.startsWith("#!") ? 1 : 0;
  while (i < lines.length && lines[i].trim() === "") i++;
  let n = 0;
  let inBlock = false;
  for (; i < lines.length; i++) {
    const trimmed = lines[i].trim();
    if (inBlock) {
      n++;
      if (trimmed.includes("*/")) inBlock = false;
    } else if (trimmed.startsWith("//")) n++;
    else if (trimmed.startsWith("/*")) {
      n++;
      inBlock = !trimmed.includes("*/");
    } else break;
  }
  return n;
}

function headerRoots(): string[] {
  return readdirSync(join(REPO, "apps"), { withFileTypes: true })
    .filter((ent) => ent.isDirectory())
    .map((ent) => join(REPO, "apps", ent.name, "src"))
    .filter((path) => { try { return statSync(path).isDirectory(); } catch { return false; } });
}

function headerCounts(): Record<string, number> {
  return collectCounts(
    headerRoots(),
    (rel) => /\.(ts|tsx)$/.test(rel)
      && !HEADER_EXCLUDE.some((pattern) => pattern.test(rel))
      && !rel.includes("/tests/")
      && !/\.(test|spec)\.(ts|tsx)$/.test(rel),
    leadingHeaderLines,
    true,
  );
}

const HEADER_RATCHET: RatchetSpec = {
  baselineFile: HEADER_BASELINE_FILE,
  updateFlag: "--update-header-baseline",
  // An unbaselined file may sit no lower than the minimum itself.
  freshAllowance: HEADER_MIN_LINES,
  guardFloor: 0,
  // Compare every counted file; only the snapshot narrows to offenders so
  // fully-headered files never enter the baseline JSON.
  snapshot: (n) => n < HEADER_MIN_LINES,
  regressed: (n, allowed) => n < allowed,
  text: (n, allowed) =>
    `file opens with ${n === 0 ? "no WHY header" : `a ${n}-line header`} (need ≥${HEADER_MIN_LINES}; grandfathered at ${allowed}) — add the mandatory file-header comment`,
  rule: "headers: every source file opens with a WHY-file-header comment (ratcheted)",
  memory: "CLAUDE.md — coding standards",
};

const violations = [
  ...runPatternChecks(),
  ...runColorFallbackCheck(),
  ...runHardcodedFallbackCheck(),
  ...runRatchet(rawCounts(), RAW_RATCHET),
  ...runRatchet(fileSizes(), SIZE_RATCHET),
  ...runRatchet(consoleCounts(), CONSOLE_RATCHET),
  ...runRatchet(headerCounts(), HEADER_RATCHET),
];
if (violations.length === 0) {
  console.log("lint-roost: 0 violations");
  process.exit(0);
}

console.log(`lint-roost: ${violations.length} violations\n`);
for (const v of violations) {
  console.log(`${v.file}:${v.line}`);
  console.log(`  ${v.text}`);
  console.log(`  rule: ${v.rule}`);
  console.log(`  memory: ${v.memory}`);
  console.log("");
}
process.exit(1);
