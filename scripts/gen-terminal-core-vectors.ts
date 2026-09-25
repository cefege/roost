// Generates the terminal-core conformance family.
//
// Two oracles, and which one a vector came from is recorded in the vector
// itself, because they answer different questions.
//
// `xterm` — the byte stream is driven through `@xterm/headless` and the
// expected state is what it produced. xterm is an independent terminal
// implementation, so a vector from it says something a Rust test that asserts
// "whatever the Rust core does" cannot: it says the Rust core agrees with a
// terminal that has nothing to do with it.
//
// `patch` — the expected state is written by hand from
// `scripts/wterm-0.5.0-roost.patch`, and the vector names the hunk. These are
// the behaviours the v2 Zig core was patched to have and xterm does not
// share; an xterm-derived vector would record the wrong answer. The generator
// still drives xterm and prints what it did, so the difference between the
// patched behaviour and the unpatched one is visible rather than asserted.
//
// Run: bun scripts/gen-terminal-core-vectors.ts
// The output is committed; regenerate only when a case is added or its bytes
// change, and diff the result — a vector that moved without its bytes moving
// is a behaviour change and deserves a commit message that says so.

import { mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Terminal } from "@xterm/headless";

const REPO = join(dirname(fileURLToPath(import.meta.url)), "..");
const OUT_DIR = join(REPO, "protocol", "conformance", "terminal-core");

type Oracle =
  /** Derived by driving `@xterm/headless`, an independent implementation. */
  | "xterm"
  /** Hand-written from `scripts/wterm-0.5.0-roost.patch`; the vector names the hunk. */
  | "patch"
  /**
   * The v2 Zig core was patched to do something `alacritty_terminal` does
   * not, and the gap is not yet closed. `expected` records what this core does
   * TODAY and `diverges_from_v2` records what v2 did, so the difference is
   * asserted and greppable instead of silently wrong. When a divergence is
   * closed, this vector fails and the case is promoted to `patch`.
   */
  | "known-divergence";

interface Expected {
  viewport: string[];
  scrollback: string[];
  cursor: { row: number; col: number };
  alt_screen: boolean;
  modes: {
    cursor_keys_app: boolean;
    bracketed_paste: boolean;
    mouse_tracking: 0 | 1000 | 1002;
    mouse_sgr: boolean;
    focus_events: boolean;
  };
}

interface Case {
  name: string;
  /** What the bytes prove. Written into the vector so a reader does not have to
   *  infer it from the diff. */
  about: string;
  cols: number;
  rows: number;
  /** Byte chunks, fed in order. A split matters: a terminal that only
   *  reassembles correctly across chunk boundaries is a different claim. */
  chunks: string[];
  oracle: Oracle;
  /** For `patch` cases, the hunk in the v2 patch this pins. */
  patch?: string;
  /** For `patch` cases, the state the patch produces, written by hand. */
  expect?: Expected;
  /** For `known-divergence` cases, what the v2 core did instead. */
  diverges_from_v2?: string;
  /** A resize after the chunks, which is a separate wire event from a write. */
  resize?: { cols: number; rows: number };
}

const ESC = "\x1b";
const CSI = `${ESC}[`;

const CASES: Case[] = [
  {
    name: "plain-text",
    about: "Text lands where it was written, and a short row keeps the columns it used.",
    cols: 20,
    rows: 4,
    chunks: ["hello"],
    oracle: "xterm",
  },
  {
    name: "wrapped-lines",
    about: "A line that exceeds the width wraps to the next row rather than being truncated.",
    cols: 10,
    rows: 3,
    chunks: ["abcdefghijklmno"],
    oracle: "xterm",
  },
  {
    name: "wide-glyph-columns",
    about:
      "A double-width glyph occupies two terminal columns, so a row of three " +
      "glyphs spans six columns and a narrow character after it is at column six.",
    cols: 12,
    rows: 3,
    chunks: ["中文文中"],
    oracle: "xterm",
  },
  {
    name: "wide-glyph-at-the-margin",
    about:
      "A wide glyph whose lead lands in the last column is not split across the " +
      "margin, which is what leaves the row one column longer than the text.",
    cols: 7,
    rows: 3,
    chunks: ["ab中cd"],
    oracle: "xterm",
  },
  {
    name: "combining-cluster",
    about:
      "A base character and its combining mark are one cell of two code points, " +
      "and occupy one terminal column.",
    cols: 10,
    rows: 3,
    chunks: ["éx"],
    oracle: "xterm",
  },
  {
    name: "scrolled-into-history",
    about:
      "Lines pushed off the top of a full viewport become history, oldest first, " +
      "and the viewport keeps the newest rows.",
    cols: 10,
    rows: 3,
    chunks: ["one\r\ntwo\r\nthree\r\nfour\r\n"],
    oracle: "xterm",
  },
  {
    name: "clear-and-erase",
    about: "ED and EL leave the rest of the row and the rest of the screen alone.",
    cols: 10,
    rows: 3,
    chunks: ["abcdefghij", CSI + "2J", CSI + "1;3H", CSI + "K"],
    oracle: "known-divergence",
    diverges_from_v2:
      "A write into the last column leaves the wrap PENDING, and this core "
      + "resolves it before dispatching a CSI. ED therefore scrolls the filled row "
      + "into history, where the reference clears it in place. The history a "
      + "client sees is shifted by a line the reference never created.",
    expect: {
      viewport: ["", "", ""],
      scrollback: ["abcdefghij"],
      cursor: { row: 0, col: 2 },
      alt_screen: false,
      modes: {
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: 0,
        mouse_sgr: false,
        focus_events: false,
      },
    },
  },
  {
    name: "cursor-positioning",
    about: "CUP and relative motion land the cursor where they are told to.",
    cols: 12,
    rows: 4,
    chunks: [CSI + "3;5H", "X", CSI + "A", "Y", CSI + "2B", "Z"],
    oracle: "xterm",
  },
  {
    name: "insert-and-delete-lines",
    about: "IL and DL move whole lines within the screen and scroll when they run out.",
    cols: 10,
    rows: 4,
    chunks: ["a", CSI + "2;1H", "b", CSI + "1;1H", CSI + "L", CSI + "2M"],
    oracle: "known-divergence",
    diverges_from_v2:
      "An insert-lines at the top of the screen pushes the scrolled-off row "
      + "into history here. The reference scrolls the viewport without creating "
      + "history, so the two disagree about where a client's history begins.",
    expect: {
      viewport: ["b", "", "", ""],
      scrollback: ["", "a"],
      cursor: { row: 0, col: 0 },
      alt_screen: false,
      modes: {
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: 0,
        mouse_sgr: false,
        focus_events: false,
      },
    },
  },
  {
    name: "erase-characters",
    about: "ECH blanks characters without moving anything else on the row.",
    cols: 12,
    rows: 3,
    chunks: ["abcdefghijkl", CSI + "1;4H", CSI + "3X"],
    oracle: "xterm",
  },
  {
    name: "modes-reach-the-frame",
    about:
      "DECCKM, bracketed paste, mouse reporting and focus reporting each read " +
      "back as the mode the emitter puts on the wire.",
    cols: 12,
    rows: 3,
    chunks: [`${CSI}?1h`, `${CSI}?2004h`, `${CSI}?1000h`, `${CSI}?1006h`, `${CSI}?1004h`],
    // Hand-written, not xterm-derived: `@xterm/headless` does not report
    // these modes, so the oracle would record them all as off and the vector
    // would assert the opposite of the truth.
    oracle: "patch",
    expect: {
      viewport: ["", "", ""],
      scrollback: [],
      cursor: { row: 0, col: 0 },
      alt_screen: false,
      modes: {
        cursor_keys_app: true,
        bracketed_paste: true,
        mouse_tracking: 1000,
        mouse_sgr: true,
        focus_events: true,
      },
    },
  },
  {
    name: "alt-screen-entry",
    about: "Entering the alternate screen reports it and paints the alternate grid.",
    cols: 12,
    rows: 3,
    chunks: ["primary", `${CSI}?1049h`, `${CSI}2J`, `${CSI}H`, "alt"],
    oracle: "xterm",
  },
  {
    name: "osc-8-link",
    about:
      "An OSC 8 link is a run of cells carrying one destination; the run ends at " +
      "the closing OSC 8 and the text after it carries no link.",
    cols: 20,
    rows: 3,
    chunks: [
      `${ESC}]8;;https://example.test/doc${ESC}\\linked${ESC}]8;;${ESC}\\ plain`,
    ],
    oracle: "xterm",
  },
  {
    name: "deferred-wrap",
    about:
      "A character written into the last column leaves the wrap PENDING: a " +
      "control character must not also wrap. The v2 Zig core clears the pending " +
      "wrap and pins the cursor to the last column before the line feed, so the " +
      "cursor ends the row in the last column and not in the first. xterm wraps " +
      "to the next row at column 0, which is the difference this patch exists to " +
      "close — scripts/wterm-0.5.0-roost.patch, processByte LF branch.",
    cols: 6,
    rows: 3,
    chunks: ["abcdef", "\n"],
    oracle: "patch",
    patch: "processByte: clear wrap_pending and pin cursor_col to cols-1 before the line feed",
    expect: {
      // The text stays where it was written. Deferred wrap is about WHERE THE
      // CURSOR GOES on the next control character, not about moving the text.
      viewport: ["abcdef", "", ""],
      scrollback: [],
      cursor: { row: 1, col: 5 },
      alt_screen: false,
      modes: {
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: 0,
        mouse_sgr: false,
        focus_events: false,
      },
    },
  },
  {
    name: "cursor-margins-clamp",
    about:
      "With DECSTBM set to rows 2..4, CUU stops at the top margin and CUD stops " +
      "at the bottom margin instead of at the screen edges. scripts/" +
      "wterm-0.5.0-roost.patch, cursorUp/cursorDown.",
    cols: 10,
    rows: 6,
    chunks: [`${CSI}2;4r`, `${CSI}3;1H`, `${CSI}9A`, "top", `${CSI}3;1H`, `${CSI}9B`],
    oracle: "known-divergence",
    patch: "cursorUp clamps to scroll_top and cursorDown to scroll_bottom - 1",
    expect: {
      // What alacritty_terminal does today: the margins are not honoured by
      // relative motion, so "top" lands on the screen's first row and the
      // cursor runs past the bottom margin.
      viewport: ["top", "", "", "", "", ""],
      scrollback: [],
      cursor: { row: 5, col: 0 },
      alt_screen: false,
      modes: {
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: 0,
        mouse_sgr: false,
        focus_events: false,
      },
    },
    diverges_from_v2:
      "The v2 patch clamps cursorUp/cursorDown to the DECSTBM margins. alacritty_terminal "
      + "stops at the screen edges, ignoring the scroll region for relative motion.",
  },
  {
    name: "vpa-ignores-margins",
    about:
      "VPA is absolute on the SCREEN, not relative to the margins, so with " +
      "margins 2..4 a VPA to row 4 lands on screen row 3 where a CUD of the " +
      "same distance stops at the bottom margin. scripts/wterm-0.5.0-roost.patch, " +
      "CSI 'e' is routed to cursorDownToScreen rather than cursorDown.",
    cols: 10,
    rows: 6,
    chunks: [`${CSI}2;4r`, `${CSI}1;1H`, `${CSI}4e`],
    oracle: "known-divergence",
    patch: "CSI e routes to cursorDownToScreen, which is bounded by the screen and not by the margins",
    expect: {
      // What alacritty_terminal does today: VPA stops at the bottom margin.
      viewport: ["", "", "", "", "", ""],
      scrollback: [],
      cursor: { row: 4, col: 0 },
      alt_screen: false,
      modes: {
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: 0,
        mouse_sgr: false,
        focus_events: false,
      },
    },
    diverges_from_v2:
      "The v2 patch routes CSI e to cursorDownToScreen, bounded by the SCREEN. "
      + "alacritty_terminal makes VPA margin-relative, so it lands on the bottom margin.",
  },
  {
    name: "alt-grid-survives-a-shrink",
    about:
      "The alternate screen is TOP-ANCHORED: shrinking its height discards from " +
      "the bottom and leaves the content where it was, where a primary " +
      "viewport would scroll to keep the cursor visible. " +
      "scripts/wterm-0.5.0-roost.patch, resizeGrid's top_anchored argument.",
    cols: 10,
    rows: 4,
    chunks: [`${CSI}?1049h`, `${CSI}2J`, `${CSI}H`, "aaa", "\r\nbbb", "\r\nccc", "\r\nddd"],
    resize: { cols: 10, rows: 2 },
    oracle: "known-divergence",
    patch: "resizeGrid passes top_anchored for the alternate grid, clamping the cursor after discarding the bottom",
    expect: {
      // What alacritty_terminal does today: the bottom rows survive.
      viewport: ["ccc", "ddd"],
      scrollback: [],
      cursor: { row: 1, col: 3 },
      alt_screen: true,
      modes: {
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: 0,
        mouse_sgr: false,
        focus_events: false,
      },
    },
    diverges_from_v2:
      "The v2 patch is TOP-ANCHORED on the alternate screen: a shrink discards from the "
      + "bottom and leaves the content where it was. alacritty_terminal keeps the LAST rows.",
  },
  {
    name: "sgr-mouse-report-is-inert",
    about:
      "An SGR-1006 mouse report shares private-mode markers with the Kitty " +
      "keyboard protocol. Without the patch it falls through and runs as " +
      "delete-lines or SGR, corrupting the screen; the patch makes anything " +
      "after CSI < or CSI = inert. scripts/wterm-0.5.0-roost.patch, csi " +
      "private-marker guard.",
    cols: 12,
    rows: 3,
    chunks: ["keepme", `${CSI}<0;5;5M`],
    oracle: "patch",
    patch: "csi: return early for a '<' or '=' private marker so a mouse report is never run as another sequence",
    expect: {
      viewport: ["keepme", "", ""],
      scrollback: [],
      cursor: { row: 0, col: 6 },
      alt_screen: false,
      modes: {
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: 0,
        mouse_sgr: false,
        focus_events: false,
      },
    },
  },
];

interface XtermView {
  viewport: string[];
  scrollback: string[];
  cursor: { row: number; col: number };
  alt_screen: boolean;
  modes: Expected["modes"];
}

async function driveWithXterm(entry: Case): Promise<XtermView> {
  const terminal = new Terminal({
    cols: entry.cols,
    rows: entry.rows,
    allowProposedApi: true,
    scrollback: 10_000,
  });
  for (const chunk of entry.chunks) {
    // `write` parses on a later tick, so the callback is the only point at
    // which the buffer reflects the bytes.
    await new Promise<void>((resolve) => terminal.write(chunk, resolve));
  }
  if (entry.resize) terminal.resize(entry.resize.cols, entry.resize.rows);

  const buffer = terminal.buffer.active;
  // A line past the end of the buffer is a blank line, not a crash: a resize
  // can leave the buffer shorter than the grid it is asked to describe, and the
  // vector for that case is hand-written from the patch anyway.
  const rowText = (row: number): string =>
    buffer.getLine(row)?.translateToString(true).trimEnd() ?? "";
  const scrollback: string[] = [];
  for (let row = 0; row < buffer.baseY; row++) scrollback.push(rowText(row));
  const viewport: string[] = [];
  const end = Math.min(buffer.baseY + entry.rows, buffer.length);
  for (let row = buffer.baseY; row < end; row++) viewport.push(rowText(row));
  // xterm names mouse tracking; the wire names the mode. Anything-motion is
  // folded away by the core, exactly as the v2 core folded it.
  const tracking = terminal.modes.mouseTracking;
  const mouseTracking = tracking === "button" ? 1000 : tracking === "any" ? 1000 : 0;
  return {
    viewport,
    scrollback,
    cursor: { row: buffer.cursorY, col: buffer.cursorX },
    alt_screen: buffer.type === "alternate",
    modes: {
      // Coerced: xterm reports an unset mode as `undefined`, which
      // `JSON.stringify` drops, and a vector missing a key is a vector the
      // runner cannot read.
      cursor_keys_app: terminal.modes.applicationCursorKeysMode === true,
      bracketed_paste: terminal.modes.bracketedPasteMode === true,
      mouse_tracking: mouseTracking as 0 | 1000,
      mouse_sgr: terminal.modes.mouseSgrEncoding === true,
      focus_events: terminal.modes.focusEvent === true,
    },
  };
}

function encodeChunks(entry: Case): string[] {
  return entry.chunks.map((chunk) => Buffer.from(chunk, "utf8").toString("base64"));
}

async function main(): Promise<void> {
  mkdirSync(OUT_DIR, { recursive: true });
  for (const stale of readdirSync(OUT_DIR)) {
    if (stale.endsWith(".json")) rmSync(join(OUT_DIR, stale));
  }
  const written: string[] = [];
  for (const entry of CASES) {
    const xterm = await driveWithXterm(entry);
    const expected: Expected =
      entry.oracle === "xterm" ? xterm : (entry.expect as Expected);
    if (entry.oracle === "known-divergence") {
      console.log(`  ${entry.name}: KNOWN DIVERGENCE — ${entry.diverges_from_v2}`);
    }
    if (entry.oracle === "patch") {
      const same =
        JSON.stringify(xterm.viewport) === JSON.stringify(expected.viewport) &&
        xterm.cursor.row === expected.cursor.row &&
        xterm.cursor.col === expected.cursor.col;
      console.log(
        `  ${entry.name}: patched=${same ? "SAME as xterm" : "DIFFERS from xterm"} ` +
          `(xterm cursor ${xterm.cursor.row},${xterm.cursor.col}; ` +
          `patched cursor ${expected.cursor.row},${expected.cursor.col})`,
      );
    }
    const vector = {
      name: entry.name,
      about: entry.about,
      oracle: entry.oracle,
      ...(entry.patch ? { patch: entry.patch } : {}),
      ...(entry.diverges_from_v2 ? { diverges_from_v2: entry.diverges_from_v2 } : {}),
      cols: entry.cols,
      rows: entry.rows,
      ...(entry.resize ? { resize: entry.resize } : {}),
      chunks: encodeChunks(entry),
      expected,
    };
    writeFileSync(join(OUT_DIR, `${entry.name}.json`), `${JSON.stringify(vector, null, 2)}\n`);
    written.push(entry.name);
  }
  console.log(`\nwrote ${written.length} vectors to protocol/conformance/terminal-core/`);

  // A vector whose bytes round-trip must be byte-identical after a write, or
  // the Rust runner is being handed something the generator did not intend.
  for (const name of written) {
    const vector = JSON.parse(readFileSync(join(OUT_DIR, `${name}.json`), "utf8"));
    for (const [index, encoded] of (vector.chunks as string[]).entries()) {
      const decoded = Buffer.from(encoded, "base64").toString("utf8");
      if (Buffer.from(decoded, "utf8").toString("base64") !== encoded) {
        throw new Error(`${name} chunk ${index} does not survive base64`);
      }
    }
  }
}

await main();
