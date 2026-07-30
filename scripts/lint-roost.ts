#!/usr/bin/env bun
// scripts/lint-roost.ts — mechanical enforcement of CLAUDE.md L11
// RECURRING-FAILURE-INDEX. Run pre-commit + pre-push.
//
// Exit 0 = clean. Exit 1 = at least one violation (printed with file:line +
// memory pointer). Add a new check by appending to CHECKS below.
//
// Run: bun scripts/lint-roost.ts
// CI:   bun scripts/lint-roost.ts || exit 1

import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
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
    memory: "themeTokens.ts secondary-container (plan tranquil-munching-wreath)",
    files: /apps\/web\/src\/.*\.tsx$/,
    pattern: /var\(--(bg|border)-selected\)/,
  },
  {
    rule: "L11: Solid setStore(key, fn → newRecord) on a Record silently no-ops",
    memory: "feedback_solid_setstore_record_replace.md",
    // Any apps/web/src file that imports setRootStore can ship the
    // pattern. Originally scoped to sync/projector but the bug also
    // hit pair-refresh in a component and an ad-hoc fix in MainPane —
    // scan all of apps/web/src.
    files: /apps\/web\/src\/.*\.(ts|tsx)$/,
    pattern: /setRootStore\(\s*["'][a-z_]+["']\s*,\s*\([^)]*\)\s*=>\s*\{/,
  },
  {
    rule: "L11: CellTerminal must render inside <For> deck, never <Show> (remount on nav loses scrollback)",
    memory: "feedback_persistent_terminal_deck.md",
    files: /apps\/web\/src\/components\/MainPane\.tsx$/,
    // <Show ...>{(session) => <CellTerminal …  (Terminal.tsx deleted in the
    // cell-shipping cutover; the deck now hosts CellTerminal).
    pattern: /<Show[^>]*>\s*\{[^}]*=>\s*<CellTerminal\b/,
  },
  {
    rule: "L11: never read props.* inside an onCleanup callback",
    memory: "feedback_no_props_read_in_oncleanup.md",
    files: /apps\/web\/src\/.*\.tsx$/,
    // crude: onCleanup(...)... props.foo within the same function body
    pattern: /onCleanup\s*\(\s*\(?\)?\s*=>\s*\{[^}]*props\./,
  },
  {
    rule: "L11: build-keeper.sh must include --external=node-pty",
    memory: "feedback_worker_deploy_macos_repairs.md",
    files: /apps\/worker\/scripts\/build-keeper\.sh$/,
    // pattern that should NOT match — invert by failing on absence below
    ok: (file, _lineIdx, lines) =>
      lines.some((l) => l.includes("--external=node-pty")),
  },
  {
    rule: "L11: apps/worker/src/keeper/package.json must declare commonjs",
    memory: "feedback_worker_deploy_macos_repairs.md",
    files: /apps\/worker\/src\/keeper\/package\.json$/,
    ok: (_file, _lineIdx, lines) =>
      lines.join("\n").includes('"type":"commonjs"') ||
      lines.join("\n").includes('"type": "commonjs"'),
  },
  // L11 ws-server.ts kill-ack rule retired in phase-24d-1 — the file is
  // deleted along with the legacy browser↔worker inbound WSS. Kill
  // routing now goes browser → sessions.kill mutation → coord →
  // CoordWorkerDownstream browser-command → SessionManager.kill →
  // closed SessionEvent fanout. The ack mechanism is the
  // pending-rpcs map + the closed event projected through the
  // sessions.events tRPC subscription.
  {
    rule: "L11: sidebar data-selected must be URL-driven, never sessions().length",
    memory: "feedback_selected_means_url_match_not_has_children.md",
    files: /apps\/web\/src\/components\/sidebar\/.*\.tsx$/,
    pattern: /data-selected\s*=\s*\{\s*sessions\(\)\.length/,
  },
  {
    rule: "L11: addToast kind must be 'ok' | 'warn' | 'err' (no 'info', 'success' etc)",
    memory: "feedback_toast_kind_must_be_in_union.md",
    files: /apps\/web\/src\/.*\.tsx?$/,
    pattern: /addToast\([^,)]+,\s*"(?!ok"|warn"|err")[a-z]+"/,
  },
  {
    // commit 4c05a8a4 — KeeperClient.kill destroyed the UDS socket
    // immediately after writing KillChild, so the keeper's Exit
    // frame arrived at a dead receiver. Result: closedByKeeper
    // never fired → no "closed" event in coord → SPA never removed
    // the row. Any sequence of `.write(...)` followed by
    // `.destroy()` on the same socket in keeper/client.ts loses
    // the queued bytes the same way.
    rule: "L11: socket.write then socket.destroy() loses queued binary frames",
    memory: "feedback_keeper_destroy_after_write.md",
    files: /apps\/worker\/src\/keeper\/client\.ts$/,
    ok: (_file, _i, lines) => {
      const txt = lines.join("\n");
      // Flag any `kill()` whose body still chains `this.socket.destroy()`.
      // The fix replaces destroy() with leaving the socket open for the
      // keeper's Exit frame to flow back.
      const m = txt.match(/kill\s*\(\s*\)\s*:\s*void\s*\{[\s\S]*?\}/);
      if (!m) return true;
      return !/this\.socket\.destroy\s*\(/.test(m[0]);
    },
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
    memory: "feedback_no_force_doRender_in_byte_handler.md",
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
    // commit 14fe6c09 — worker main.ts `case "resize"` fell through
    // to the fire-and-forget log-and-return stub for weeks. claude /
    // vim / less rendered at the keeper default 220×50 while wterm
    // displayed at viewport width → wrap chaos in every TUI. The fix:
    // give resize its own handler block that calls sessionMgr.resize.
    // Lint guard: ensure "resize" is NOT in the fall-through chain
    // that ends in the browser_command_via_coord no-op stub.
    rule: "L11: worker case \"resize\" must have its own handler block",
    memory: "feedback_resize_fallthrough_to_noop.md",
    files: /apps\/worker\/src\/main\.ts$/,
    ok: (_file, _i, lines) => {
      const txt = lines.join("\n");
      // Find the index of `case "resize":` and look at what follows.
      // OK if a `{` (own block) appears before the next `case` token.
      // BAD if the next non-whitespace token is `case "...":` — i.e.
      // resize is falling through into another case's body.
      const m = txt.match(/case\s+["']resize["']\s*:([\s\S]*?)(?=case\s+["']|default\s*:|\n\s{4,}\}\s*\n)/);
      if (!m) return true; // resize handler missing entirely — not our concern here
      const body = m[1] ?? "";
      // Acceptable shape: resize routes to a real handler. Post multi-viewer
      // SCD model the call is sessionMgr.claimViewport (cols/rows=0 = withdraw);
      // the pre-SCD name was sessionMgr.resize. Either is a real handler.
      if (/sessionMgr\.(resize|claimViewport)/.test(body)) return true;
      // Falls through with no handler → the bug.
      return false;
    },
  },
  // L11 SSE-rehydrate rule retired in the cell-shipping cutover: the tRPC v11
  // SSE transport that delivered Uint8Array as a {"0":x,…} JSON object is gone
  // — PTY bytes now arrive as proto binary over the Connect Sync stream, so the
  // instanceof-drops-bytes class is structurally impossible. (Terminal.tsx, the
  // rule's target file, is also deleted.)
  {
    // att1f — attachment flow must go through PTY round-trip.
    // wterm.write* paints into the local buffer without traversing the
    // worker, so claude / the underlying tool never SEES the typed path.
    // The user would see the path on screen but tools couldn't read it.
    rule: "att1: attachment code must NOT call wterm.write* — paint via PTY",
    memory: "docs/archive/phase-att1.md",
    files: /apps\/web\/src\/(lib\/attachments|components\/AttachmentChip)\.(ts|tsx)$/,
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
        "apps/worker/src/transport/CoordLink.ts",
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
    memory: "feedback_safejsonparse_on_bus_publish_path.md",
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
    memory: "feedback_caller_fp_null_audit_log.md",
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
    memory: "feedback_bun_terminal_needs_explicit_TERM.md",
    files: /apps\/worker\/src\/keeper\/keeper-frame-handler\.ts$/,
    ok: (_file, _i, lines) => /TERM:\s*["']xterm/.test(lines.join("\n")),
  },
  {
    // The bug: wterm renders scrollback as .term-scrollback-row DOM elements;
    // without `.wterm { overflow-y: auto }` the rows are clipped and the user
    // cannot scroll up to see history ("THERE IS NO SCROLL"). The fix is this
    // one CSS rule — NOT switching terminal cores. Pin it to the .wterm block.
    rule: "L11: .wterm must keep overflow-y: auto (scrollback rows clip otherwise — do NOT switch cores)",
    memory: "feedback_no_force_doRender_in_byte_handler.md",
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
    memory: "feedback_no_connect_node_compression_under_bun.md",
    files: /apps\/coord\/src\/.*\.ts$/,
    pattern: /from\s+["']node:zlib["']|require\(\s*["']node:zlib["']\s*\)/,
  },
];

// ───────────────────────────────────────────────────────────────────────

const SKIP_DIRS = new Set([
  "node_modules", ".git", "dist", ".turbo", "target",
  "coverage", "test-results", ".claude",
]);

function* walk(dir: string): Generator<string> {
  for (const ent of readdirSync(dir, { withFileTypes: true })) {
    if (SKIP_DIRS.has(ent.name)) continue;
    const p = join(dir, ent.name);
    if (ent.isDirectory()) yield* walk(p);
    else if (ent.isFile()) yield p;
  }
}

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
          memory: "feedback_no_hardcoded_color_fallbacks.md",
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
// See feedback_no_hardcoded_color_fallbacks.md.
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
          memory: "feedback_no_hardcoded_color_fallbacks.md",
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

function collectRawCounts(): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const file of walk(join(REPO, "apps/web/src"))) {
    const rel = file.slice(REPO.length).replace(/^\/+/, "");
    if (!/\.(tsx|ts|css)$/.test(rel)) continue;
    if (rel.endsWith(".test.tsx") || rel.endsWith(".test.ts") || rel.includes("/e2e/")) continue;
    if (RAW_VALUE_ALLOW.has(rel)) continue;
    let text: string;
    try { text = readFileSync(file, "utf8"); } catch { continue; }
    const n = rawValueLineCount(text);
    if (n > 0) counts[rel] = n;
  }
  return counts;
}

function runRawValueCheck(): Violation[] {
  const counts = collectRawCounts();
  let baseline: Record<string, number> = {};
  try { baseline = JSON.parse(readFileSync(RAW_BASELINE_FILE, "utf8")) as Record<string, number>; }
  catch { baseline = {}; }
  const out: Violation[] = [];
  for (const [rel, n] of Object.entries(counts)) {
    const allowed = baseline[rel] ?? 0;
    if (n > allowed) {
      out.push({
        file: rel, line: 1,
        text: `${n} raw hex/rgb/px-font value lines (baseline ${allowed}) — reference a theme token instead`,
        rule: "design: no NEW raw color/px-font values — use --md-*/--surface-*/--text-* + the type ramp (ratcheted)",
        memory: "design-system-phase1",
      });
    }
  }
  return out;
}

// `--update-design-baseline`: rewrite the ratchet baseline from the current
// tree (run after migration lowers counts). Prints + exits before the checks.
if (process.argv.includes("--update-design-baseline")) {
  const counts = collectRawCounts();
  const sorted = Object.fromEntries(Object.entries(counts).sort(([a], [b]) => a.localeCompare(b)));
  writeFileSync(RAW_BASELINE_FILE, JSON.stringify(sorted, null, 2) + "\n");
  const total = Object.values(counts).reduce((s, n) => s + n, 0);
  console.log(`lint-roost: wrote design-raw-baseline.json — ${Object.keys(counts).length} files, ${total} raw-value lines`);
  process.exit(0);
}

// ───────────────────────────────────────────────────────────────────────

const violations = [
  ...runPatternChecks(),
  ...runColorFallbackCheck(),
  ...runHardcodedFallbackCheck(),
  ...runRawValueCheck(),
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
