# Path B — server-side VT parser + grid snapshot

Replaces Path A's raw-byte-replay snapshot (`getScrollbackSince`) with a
parsed grid + scrollback line buffer maintained on the worker. The
SPA's wterm becomes a thin renderer over the worker's authoritative
state (a server-side-parse model).

**Why we landed here.** Path A (ssb0-ssb-altmode-unconditional) shipped
and works for byte-level correctness. But raw-byte snapshots cannot
satisfy BOTH "no alt-screen wallpaper pollution" AND "scrollback is
real conversation history" simultaneously — claude exits alt-screen
mid-session to dump answers into scrollback, and replaying those mode
toggles is what creates the wallpaper. Strip the toggles and lose
scrollback. Keep them and lose visual sanity. Documented in
`project_seqno_splice_path_a_chosen.md` (2026-06-14 UPDATE) and
research workflow (99 agents, 17 sources): every n-tier terminal
project that's solved this parses VT server-side.

**Why not an embedded-native approach.** A native macOS terminal with the
renderer embedded in-process has no network seam between parser and
renderer. Not applicable to browser-based Roost without a full-product
rewrite.

## Invariants (target end state)

1. Worker owns a `TerminalState` per session: parsed grid (cells +
   attrs), cursor position, mode flags (alt-screen, cursor-visible,
   wrap, etc.), and a separate scrollback buffer of `Row[]` (default
   8000 rows) populated WHEN claude/vim/less exits alt-screen and
   writes to main-screen.
2. VT parser runs on EVERY byte the keeper outputs. Parser updates
   `TerminalState`. Live bytes ALSO still forward upstream via the
   existing seqno-tagged byte path (Path A infra reused).
3. New RPC `getGridSnapshot(session_id)` returns
   `{ cells, attrs, cursor, modes, scrollback_rows, head_seq }`.
4. SPA Terminal.tsx mount sequence:
   a. Call `getGridSnapshot(session_id)` → receive parsed state +
      head_seq.
   b. Render snapshot directly into wterm via `@xterm/addon-serialize`
      deserialize OR by emitting synthesized minimal ANSI that
      reproduces the grid + scrollback. Approach #2 is the proven one.
   c. lastSeq = response.head_seq. From here, live bytes via firehose
      (with seqno tagging per Path A) get written to wterm as-is.
5. Multi-viewer: every browser independently calls getGridSnapshot at
   mount; live bytes fan out via the existing firehose; each browser's
   wterm stays in sync because (a) initial state is identical, (b)
   live bytes are deterministic ANSI that wterm interprets the same
   way as the worker's parser.
6. Worker-restart recovery: TerminalState rebuilt by replaying the
   keeper's ring through the parser. ~8MB of bytes → grid + scrollback
   in <100ms.

## Phase breakdown (pb = path-b)

| Phase | Scope | Files (approximate) | Verify |
|---|---|---|---|
| pb1 | Pick VT parser + integration mode. Candidates: (a) `vt-rs` via N-API binding from Bun's worker process, (b) port of `xterm.js`'s parser to Bun TypeScript, (c) a libvterm-compat C lib via Bun:ffi, (d) write a minimal VT-100/xterm subset parser sufficient for what claude/vim/less actually emit. Pick + write a short ADR. | new `FEATURES/PATHB-PARSER-CHOICE.md` | ADR approved |
| pb2 | TerminalState type + parser integration in worker. Per-session: parser instance, grid (cells × cols × rows), cursor, mode flags, scrollback Row[] (push when alt-screen exits to main-screen). Parse every keeper chunk; existing seqno + byte forwarding untouched. | `apps/worker/src/session-manager.ts`, new `apps/worker/src/terminal-state.ts` | unit tests for: alt-screen enter/exit doesn't pollute scrollback; cursor positioning; CRLF; basic SGR; box drawing |
| pb3 | New `getGridSnapshot` RPC. Wire shape: `{ cells: Cell[][], cursor: {row, col, visible}, modes: {alt_screen, wrap, ...}, scrollback: Row[], head_seq: number }`. Coord forwards to worker via existing browser-command path. | `apps/shared/src/router.ts`, `apps/shared/src/wire/control.ts` (new variant `get-grid-snapshot`), `apps/coord/src/router/sessions.ts`, `apps/worker/src/main.ts` | RPC roundtrip test |
| pb4 | SPA cutover. Terminal.tsx onMount calls getGridSnapshot, deserializes into wterm. The seqno-keyed live byte path (getScrollbackSince + lastSeq tracker + writeChunk gap detection) stays for FIREHOSE GAP RECOVERY only — never used for snapshot now. Old getScrollbackSince RPC stays as fallback for one phase, deleted in pb5. | `apps/web/src/components/Terminal.tsx`, `apps/web/src/store/sync.ts` | humanchrome: mount claude session, verify no wallpaper + verify scrolling up shows real conversation answers |
| pb5 | Delete getScrollbackSince + worker scrollback ring (`appendScrollback`'s flat Uint8Array) — TerminalState replaces it. Strip alt-mode tracking helpers — parser owns mode now. Delete SPA's earlyBuf + writeChunk's blind-write path. Delete ssb-altmode-* unconditional prefix (parser handles modes correctly). | sweep via `rg getScrollbackSince\|alt_mode\|ALT_ENTER_PREFIX\|appendScrollback` | all tests green |
| pb6 | Worker-restart recovery test. Kill worker mid-session, restart, browser refresh, assert (a) terminal state matches pre-restart, (b) scrollback rows preserved, (c) firehose resumes via seqno without gap warning. | `apps/web/e2e/scenarios/pathb-worker-restart.spec.ts` | scenario passes |
| pb7 | Regression scenarios: alt-screen TUI (claude full session), plain shell (zsh prompts + ls output + scrollback), mixed (vim from shell, exit, more shell), reconnect during alt-screen, reconnect during main-screen, multi-viewer (two browsers same session). | `apps/web/e2e/scenarios/pathb-*.spec.ts` | all green |

## Reuse from Path A

DO NOT delete the seqno protocol. It still anchors:
- Live byte forwarding from worker through coord firehose to SPA
- Gap detection on firehose WS reconnect (lastSeq tracker)
- The byte-level seqno in `apps/coord/src/buses.ts` globalBytesBus
- `apps/worker/src/session-manager.ts` head_seq tracking

Path B replaces ONLY the snapshot mechanism. The live wire stays
identical. CLAUDE.md L11 "scrollback seam torn" row already covers
this — the seqno fix prevents one class of bug, parser-side state
prevents the OTHER class (mode pathology).

## Escape hatch from Path B

If Path B regresses worse than Path A's status quo (which is currently
acceptable for the "type and see live" case, just bad for scrollback):

1. **Revert to Path A.** All Path A commits stay in history under
   `phase-ssb*`. A single `git revert` of the Path B series brings us
   back to the unconditional-prefix alt-screen world.
2. **Hybrid: Path B for kind="claude" sessions, Path A for kind="shell".**
   Shell sessions are simpler (no alt-screen toggling) so byte-replay
   works fine. Only claude-and-friends need the parser.

## Out of scope for Path B (deferred)

- Mouse tracking + clipboard via VT — wterm's existing wiring covers
  this; parser doesn't need to track.
- Bidirectional input semantics — keystrokes still flow SPA → coord →
  worker → keeper as raw bytes (Path A's input path).
- Sixel / extended graphics protocols — parser ignores these; bytes pass
  through to wterm which has its own handling.
- Persisted grid across coord restart — coord doesn't own grid state,
  worker does. Coord restart is invisible to Path B.

## What previous Claude got wrong on Path A

For future-Claude reference: the Path A series spent ~90 minutes on
band-aids (alt-mode detection, TUI heuristic, unconditional prefix,
SPA-side wterm clear, strip-exits) trying to engineer around the
fundamental "alt-screen has no scrollback above viewport" constraint.
Each band-aid traded one symptom for another. The actual answer was
visible after the first failed band-aid: the structural fix is parser
on the worker. Do not repeat the band-aid loop on this surface. If a
"scrollback feels weird with claude/vim/less" bug appears post-Path B,
the answer is "look at the parser state for this session," not
"prepend another escape sequence."

## Start command for new context

```
read FEATURES/PATHB-VT-PARSE.md and start phase pb1
```
