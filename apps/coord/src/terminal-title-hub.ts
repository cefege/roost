// terminal-title-hub — coord-authoritative OSC terminal title.
//
// Parses the OSC 0/2 title sequence (`ESC ] 0 ; <title> BEL` or the window
// variant `ESC ] 2 ; <title>`, BEL- or ST-terminated) out of the SAME bytes
// coord already relays (globalBytesBus), and publishes the latest title per
// session to titleBus on CHANGE → the Sync stream fans it to every browser.
//
// Mirrors claude-status-hub (one per coord, started from createCoord), but
// needs no headless grid — the title is a discrete in-band escape, parsed
// directly from the byte stream. Replaces the dead per-browser RoostTerm.onTitle
// path (orphaned when Terminal.tsx was deleted) so the sidebar title is one
// coord-authoritative value every viewer + every Mac agrees on.
//
// Depends on: buses (globalBytesBus in, titleBus out, sessionBus reap).

import { diag } from "@roost/shared";
import { globalBytesBus, sessionBus, titleBus } from "./buses.ts";

// OSC 0 (icon+title) / 2 (window title); body is everything up to the
// terminator (BEL \x07 or ST \x1b\\), and may not contain ESC. Global so a
// chunk carrying several title changes yields the LAST (live) one.
// eslint-disable-next-line no-control-regex
const OSC_TITLE_RE = /\x1b\][02];([^\x07\x1b]*)(?:\x07|\x1b\\)/g;
// Cap the cross-chunk carry: a title that straddles a byte-stream chunk
// boundary is bridged here, but a never-terminated OSC (or unrelated OSC
// chrome like hyperlinks) must not grow unbounded. Titles are short.
const CARRY_CAP = 1024;
// Cap the published title — a buggy/hostile program emitting a giant title
// must not fan a multi-KB string to every viewer (the SPA only shows ~80).
const MAX_TITLE = 256;
// Strip C0 controls + DEL from the title body: a title with \n/\r/\t would
// break the single-line sidebar row (the body regex already excludes BEL/ESC).
// eslint-disable-next-line no-control-regex
const CONTROL_RE = /[\x00-\x1f\x7f]/g;

function sanitizeTitle(raw: string): string {
  return raw.replace(CONTROL_RE, "").slice(0, MAX_TITLE);
}

interface Entry {
  carry: string;                 // un-terminated OSC tail from the previous chunk
  decoder: TextDecoder;          // streaming UTF-8 so a multi-byte char split survives
  last: string | null;           // last published title (null = none yet)
}

const _entries = new Map<string, Entry>();

/** Current title per session — replayed to each new Sync subscriber so a fresh
 *  page load reflects the live title immediately (titleBus is publish-on-change,
 *  not backfilled). */
export function getTitleSnapshot(): Array<{ session_id: string; title: string }> {
  const out: Array<{ session_id: string; title: string }> = [];
  for (const [sid, e] of _entries) {
    if (e.last !== null) out.push({ session_id: sid, title: e.last });
  }
  return out;
}

export function startTerminalTitleHub(): () => void {
  const unsubBytes = globalBytesBus.subscribe(({ session_id, bytes }) => {
    if (bytes.byteLength === 0) return;
    let e = _entries.get(session_id);
    if (!e) {
      e = { carry: "", decoder: new TextDecoder("utf-8", { fatal: false }), last: null };
      _entries.set(session_id, e);
    }
    // Always decode (advances the streaming decoder past a split multi-byte
    // char) — but most output carries no OSC, so skip the regex + carry work
    // unless an OSC START (`ESC ]`) is present in buf (carry included).
    const buf = e.carry + e.decoder.decode(bytes, { stream: true });
    if (buf.indexOf("\x1b]") < 0) { e.carry = ""; return; }

    let latest: string | null = null;
    let lastEnd = 0;
    OSC_TITLE_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = OSC_TITLE_RE.exec(buf)) !== null) {
      latest = m[1]!;
      lastEnd = OSC_TITLE_RE.lastIndex;
    }
    // Keep whatever follows the last complete title (may hold a split OSC start),
    // bounded so chrome without a terminator can't accumulate.
    let carry = buf.slice(lastEnd);
    if (carry.length > CARRY_CAP) carry = carry.slice(-CARRY_CAP);
    e.carry = carry;

    if (latest === null) return;
    const title = sanitizeTitle(latest);
    if (title !== e.last) {
      e.last = title;
      titleBus.publish({ session_id, title });
      diag("terminal_title.change", { sid: session_id, title });
    }
  });

  // Drop a session's parser state when it closes (mirrors claude-status-hub).
  const unsubSessions = sessionBus.subscribe((ev) => {
    if (ev.kind !== "closed") return;
    _entries.delete(ev.session_id);
  });

  return () => {
    unsubBytes();
    unsubSessions();
    _entries.clear();
  };
}
