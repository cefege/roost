// scripts/lint-failure-checks.ts — the per-file pattern/predicate table
// lint-roost.ts walks the repo with. One entry per CLAUDE.md L11 row and
// docs/FAILURE-INDEX.md guard: the narrowest grep (or `ok` predicate) that
// catches the bad pattern without false positives. Consumed only by
// lint-roost.ts's runPatternChecks; depends on lint-transport-allowlists.ts
// for the two transport exemption lists.

import {
  WEB_SOCKET_CLIENT_ALLOW,
  WEB_SOCKET_LISTENER_ALLOW,
} from "./lint-transport-allowlists.ts";

export interface Check {
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

export const CHECKS: Check[] = [
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
    files: /apps\/web\/src\/components\/terminal\/CellTerminal\.tsx$/,
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
    memory: "docs/FAILURE-INDEX.md",
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
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/(web|worker)\/src\/.*\.(ts|tsx)$/,
    ok: (file, _i, lines) => {
      if (WEB_SOCKET_CLIENT_ALLOW.some((s) => file.endsWith(s))) return true;
      return !/new\s+WebSocket\s*\(/.test(lines.join("\n"));
    },
  },
  {
    rule: "phase-24: `Bun.serve({ websocket })` outside coord main and the worker's local UI door",
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/(coord|worker)\/src\/.*\.ts$/,
    ok: (file, _i, lines) => {
      if (WEB_SOCKET_LISTENER_ALLOW.some((s) => file.endsWith(s))) return true;
      return !/Bun\.serve\s*\(\s*\{[\s\S]*?\bwebsocket\s*:/.test(lines.join("\n"));
    },
  },
  {
    rule: "phase-24: module-level `let _ws` / `let _reconnectTimer` in apps/web/src/store or client",
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/web\/src\/(?:store|client)\/.*\.ts$/,
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
    files: /apps\/coord\/src\/(.+\/)?handlers-[^/]*\.ts$/,
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
    files: /apps\/coord\/src\/auth\/auth-interceptor\.ts$/,
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
    // The bug: with no overscroll policy the browser owns the edge of every
    // gesture — a drag past the end of terminal scrollback rubber-bands or
    // pull-to-refreshes the whole page on mobile, and steals gestures the
    // terminal application is supposed to receive. index.html's inline base
    // style is the only place html/body are reachable.
    rule: "L11: index.html base style must keep * { overscroll-behavior-y: none } + html,body { overflow: hidden } (else mobile drags the page)",
    memory: "docs/FAILURE-INDEX.md",
    files: /apps\/web\/index\.html$/,
    ok: (_file, _i, lines) => {
      const css = lines.join("\n");
      return /\*\s*\{[^}]*overscroll-behavior-y\s*:\s*none[^}]*\}/.test(css)
        && /html,\s*body\s*\{[^}]*overflow\s*:\s*hidden[^}]*\}/.test(css);
    },
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
