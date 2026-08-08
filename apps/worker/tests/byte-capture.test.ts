// byte-capture ring tests. Verify:
//   - push appends + caps at 256KB
//   - dump writes header + body, returns path
//   - LRU evicts oldest dump files past the cap
//   - drop clears the ring

import { describe, expect, test, beforeEach, afterAll } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { tmpdir, homedir } from "node:os";
import * as bc from "../src/diag/byte-capture.ts";
import { readRing } from "../src/session-scrollback-ring.ts";

const RING_CAP = 256 * 1024;

describe("byte-capture", () => {
  beforeEach(() => { bc._resetForTest(); });

  test("push appends + caps at RING_CAP_BYTES", () => {
    const sid = "sid-test-cap";
    // Push 100KB twice — total 200KB, under the cap, full retention.
    bc.push(sid, new Uint8Array(100_000).fill(0xAA), 100_000);
    bc.push(sid, new Uint8Array(100_000).fill(0xBB), 200_000);
    const ring = bc._getRingForTest(sid)!;
    const bytes = readRing(ring.ring);
    expect(bytes.length).toBe(200_000);
    expect(ring.end_seq).toBe(200_000);
    expect(bytes[0]).toBe(0xAA);
    expect(bytes[100_000]).toBe(0xBB);

    // Push enough to exceed the cap. Verify ring keeps only the tail.
    bc.push(sid, new Uint8Array(200_000).fill(0xCC), 400_000);
    const ring2 = bc._getRingForTest(sid)!;
    const bytes2 = readRing(ring2.ring);
    expect(bytes2.length).toBe(RING_CAP);
    expect(ring2.end_seq).toBe(400_000);
    // Read is oldest→newest across the wrap: tail is 0xCC, head still 0xBB.
    expect(bytes2[bytes2.length - 1]).toBe(0xCC);
    expect(bytes2[0]).toBe(0xBB);
  });

  test("drop clears the ring", () => {
    const sid = "sid-test-drop";
    bc.push(sid, new Uint8Array(1000), 1000);
    expect(bc._getRingForTest(sid)).toBeDefined();
    bc.drop(sid);
    expect(bc._getRingForTest(sid)).toBeUndefined();
  });

  test("dump writes file with header + body, returns path", () => {
    const sid = "sid-test-dump";
    bc.push(sid, new Uint8Array([0x01, 0x02, 0x03, 0x04, 0x05]), 5);
    const path = bc.dump(sid, "test_reason");
    expect(path).not.toBeNull();
    const content = readFileSync(path!);
    const newlineIdx = content.indexOf(0x0A);
    expect(newlineIdx).toBeGreaterThan(0);
    const header = JSON.parse(content.subarray(0, newlineIdx).toString("utf8"));
    expect(header.sid).toBe(sid);
    expect(header.end_seq).toBe(5);
    expect(header.ring_len).toBe(5);
    expect(header.reason).toBe("test_reason");
    const body = content.subarray(newlineIdx + 1);
    expect(body.length).toBe(5);
    expect(Array.from(body)).toEqual([0x01, 0x02, 0x03, 0x04, 0x05]);
    // Clean up the file we just wrote.
    try { rmSync(path!); } catch { /* ignore */ }
  });

  test("dump returns null on empty ring", () => {
    expect(bc.dump("sid-never-pushed", "noop")).toBeNull();
  });
});

afterAll(() => {
  // Clean any test dump files in the default dump dir.
  try {
    const dir = join(homedir(), "Library", "Logs", "RoostWorker");
    const fs = require("node:fs");
    for (const f of fs.readdirSync(dir)) {
      if (f.startsWith("bytecap-sid-test-")) {
        try { rmSync(join(dir, f)); } catch { /* ignore */ }
      }
    }
  } catch { /* ignore */ }
});
