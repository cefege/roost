# pb1 ADR — VT parser choice for Path B

**Status:** decided (revised 2026-06-15). **Choice: `@wterm/core` `WasmBridge` on the worker.**
Same WASM parser the SPA renders with via `@wterm/dom`. Snapshot wire shape ships
parsed cells + scrollback rows directly, not synthesized ANSI.

## Decision (one-line)

Each session on the worker owns one `@wterm/core` `WasmBridge` instance.
Every keeper PTY byte is `bridge.writeRaw(bytes)`-fed to it. The
`getGridSnapshot` RPC reads structured cells via the bridge's
`getCell` / `getCursor` / `getScrollbackCell` / `getScrollbackLineLen` /
`getScrollbackCount` / `usingAltScreen` API and ships them as wire data.
The SPA reconstructs into its own `WasmBridge` (the one `@wterm/dom`
already owns) by writing synthesized ANSI derived from those cells.

## What wterm gives us — verbatim from the package

From `apps/web/node_modules/@wterm/core/dist/terminal-core.d.ts`:

```ts
export interface TerminalCore {
  init(cols, rows); resize(cols, rows);
  writeString(str); writeRaw(data: Uint8Array);
  getCell(row, col): { char, fg, bg, flags, fgRgb?, bgRgb? };
  getCursor(): { row, col, visible };
  usingAltScreen(): boolean;
  getScrollbackCount(): number;
  getScrollbackCell(offset, col): CellData;
  getScrollbackLineLen(offset): number;
  getCols(); getRows();
  getTitle(); getResponse();
  isDirtyRow(row); clearDirty();
  getUnhandledSequences(); cursorKeysApp(); bracketedPaste();
}
```

From `src/scrollback.zig` upstream: `MAX_SCROLLBACK_LINES = 1000` — the ring
keeps the last 1000 parsed lines (NOT bytes). Each `ScrollbackLine` carries
its cells + length. Eviction is per-line, not per-byte. This is the structural
property we need: scrollback is the parsed conversation history regardless of
what alt-screen toggles the byte stream contained.

Bun load: confirmed via probe — `WasmBridge.load()` with the embedded base64
WASM works under Bun. `writeString("hello\r\nworld")` produces the expected
two rows, `usingAltScreen()` flips correctly on `ESC[?1049h/l`, flag bits
decode as bold=0x01 italic=0x04 underline=0x08 reverse=0x20 strike=0x80.
Colors quantize to 256-palette in the Zig WASM grid (no true-color round-trip).

## Candidates considered

| Candidate | Same parser as SPA? | Built-in scrollback API? | Bun? | Verdict |
|---|---|---|---|---|
| `@wterm/core` WasmBridge | YES — `@wterm/dom` uses this exact engine | YES — getScrollbackCount/Cell/LineLen | YES (probed) | **CHOSEN** |
| `@xterm/headless` + `@xterm/addon-serialize` | no — different parser family from wterm | yes (different API) | yes | rejected — parse divergence risk between worker emit and wterm reparse was the escape-hatch in the original ADR; switching the parser entirely retires that risk |
| `vt-rs` / `libvterm` via FFI | no | partial | needs N-API | rejected — second native shim multiplies deploy pain (memory `feedback_worker_deploy_macos_repairs.md`) |
| hand-rolled subset | no | no | yes | rejected — Path A band-aid lesson |

The xterm-headless ADR (prior revision, deleted) framed wterm/xterm as
"both speak xterm-compatible ANSI so roundtrip works." That's true but
buys us nothing once @wterm/core works under Bun — using the SAME engine
twice is a strictly stronger guarantee than "two compatible engines should
agree on every escape sequence." Drop the assumption, drop the failure mode.

## Wire shape (pb3, revised)

```ts
GridSnapshot = {
  cols: number,
  rows: number,
  cursor: { row, col, visible },
  alt_screen: boolean,
  // Active grid, top-to-bottom. Each row is exactly `cols` cells.
  grid: Cell[][],
  // Scrollback rows, OLDEST-to-NEWEST (matches getScrollbackCell offset 0 = oldest).
  scrollback: { cells: Cell[], len: number }[],
  head_seq: number,
};
Cell = { ch: number, fg: number, bg: number, flags: number, fgRgb?: number, bgRgb?: number };
```

SPA pb4 path: receive snapshot → walk into its `@wterm/dom` instance's
WasmBridge via a small client-side `replayInto(bridge, snapshot)` helper
that emits synthesized ANSI (cursor-home, scrollback rows + \r\n, grid
rows with absolute positioning, alt-screen toggle, final cursor). The
helper lives in `apps/web/src/lib/wterm-replay.ts`. Both sides share the
Zod wire schema in `@roost/shared`.

## Reuse from Path A

UNCHANGED: live byte forwarding + seqno protocol + firehose. Path B only
swaps the snapshot mechanism. The Path A scrollback BYTE RING + alt_mode
heuristics get stripped in pb5 once SPA is fully on getGridSnapshot.

## Escape hatch from Path B

1. Revert via `git revert` of the pb1-pb5 series → back to Path A.
2. Hybrid: WasmBridge for `kind="claude"` sessions, byte-replay for
   `kind="shell"` (shell has no alt-screen toggling, byte-replay works).

## What's deferred

- True-color SGR preservation in snapshot (WasmBridge quantizes to 256;
  live wire still carries true-color bytes so the visible session stays
  true-color after snapshot finishes loading and live bytes catch up).
- Sixel / extended terminal graphics protocols in snapshot — fall through via live wire.
- Mouse / focus / bracketed-paste in snapshot — already-modal; live wire
  re-establishes after first input.

## Anti-goals — do not do during pb2-pb7

- No second VT engine on the worker — just WasmBridge.
- No new escape-sequence prefixes (ssb-altmode-* gets ripped in pb5).
- No "raw byte log + filter" snapshot — that's Path A.
- No state in `@roost/shared` parser — shared owns only the wire Zod
  schemas; parsing lives in the worker (production) and SPA (snapshot
  consumer).

## Start command for pb2

```
read FEATURES/PATHB-PARSER-CHOICE.md and rewrite apps/worker/src/terminal-state.ts around WasmBridge
```
