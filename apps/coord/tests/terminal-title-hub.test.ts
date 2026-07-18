// Drives the coord terminal-title-hub: relayed bytes → OSC parse →
// titleBus publish. Covers BEL + ST terminators, cross-chunk splits,
// change-only publishing, and non-title OSC (hyperlink) rejection.

import { describe, it, expect } from "bun:test";
import { startTerminalTitleHub } from "../src/terminal-title-hub.ts";
import { globalBytesBus, titleBus } from "../src/buses.ts";

function collect(sessionId: string): { got: string[]; stop: () => void } {
  const got: string[] = [];
  const unsub = titleBus.subscribe((m) => { if (m.session_id === sessionId) got.push(m.title); });
  const stopHub = startTerminalTitleHub();
  return { got, stop: () => { unsub(); stopHub(); } };
}
function feed(sessionId: string, text: string): void {
  const bytes = new TextEncoder().encode(text);
  globalBytesBus.publish({ session_id: sessionId, bytes });
}

describe("terminal-title-hub", () => {
  it("parses an OSC 0 BEL-terminated title", () => {
    const sid = "title-bel";
    const c = collect(sid);
    feed(sid, "ignored output \x1b]0;my-project\x07more output");
    c.stop();
    expect(c.got).toEqual(["my-project"]);
  });

  it("parses an OSC 2 ST-terminated (ESC backslash) title", () => {
    const sid = "title-st";
    const c = collect(sid);
    feed(sid, "\x1b]2;window-name\x1b\\");
    c.stop();
    expect(c.got).toEqual(["window-name"]);
  });

  it("bridges a title split across two byte chunks", () => {
    const sid = "title-split";
    const c = collect(sid);
    feed(sid, "prefix \x1b]0;split-ti");   // OSC starts, no terminator yet
    feed(sid, "tle\x07 suffix");            // completes in the next chunk
    c.stop();
    expect(c.got).toEqual(["split-title"]);
  });

  it("publishes only on CHANGE", () => {
    const sid = "title-dedupe";
    const c = collect(sid);
    feed(sid, "\x1b]0;same\x07");
    feed(sid, "redraw \x1b]0;same\x07");   // same title again → no second publish
    feed(sid, "\x1b]0;different\x07");
    c.stop();
    expect(c.got).toEqual(["same", "different"]);
  });

  it("ignores non-title OSC (hyperlink \\x1b]8)", () => {
    const sid = "title-hyperlink";
    const c = collect(sid);
    feed(sid, "\x1b]8;;https://example.com\x07link text\x1b]8;;\x07");
    c.stop();
    expect(c.got).toEqual([]);
  });

  it("caps an oversized title to 256 chars (no multi-KB fan-out)", () => {
    const sid = "title-huge";
    const c = collect(sid);
    feed(sid, `\x1b]0;${"x".repeat(5000)}\x07`);
    c.stop();
    expect(c.got.length).toBe(1);
    expect(c.got[0]!.length).toBe(256);
  });

  it("strips C0 control chars (newline/tab) from the title", () => {
    const sid = "title-controls";
    const c = collect(sid);
    feed(sid, "\x1b]0;line1\ttab\rmid\x07");   // \t and \r inside the body
    c.stop();
    expect(c.got).toEqual(["line1tabmid"]);
  });
});
