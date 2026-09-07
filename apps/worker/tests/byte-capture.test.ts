// byte-capture ring tests. Verify:
//   - push appends + caps at 256KB
//   - dump writes header + body, returns path
//   - the dump dir is 0700 and every dump 0600, including when the dir
//     already existed with looser permissions
//   - drop clears the ring
// The dump directory is redirected per test so no case touches the real
// worker log dir or changes its permissions.

import { describe, expect, test, beforeEach, afterEach } from "bun:test";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import * as bc from "../src/diag/byte-capture.ts";
import { readRing } from "../src/session-scrollback-ring.ts";

const RING_CAP = 256 * 1024;
const SKIP_ON_WINDOWS = process.platform === "win32";

let dumpRoot: string;
let dumpDir: string;
const priorLogDir = process.env.ROOST_WORKER_LOG_DIR;

describe("byte-capture", () => {
  beforeEach(() => {
    bc._resetForTest();
    dumpRoot = mkdtempSync(join(tmpdir(), "roost-bytecap-"));
    // Nested so the dump dir itself does not exist yet: mkdtemp's own 0700
    // would make the created-by-us mode assertion vacuous.
    dumpDir = join(dumpRoot, "RoostWorker");
    process.env.ROOST_WORKER_LOG_DIR = dumpDir;
  });

  afterEach(() => {
    if (priorLogDir === undefined) delete process.env.ROOST_WORKER_LOG_DIR;
    else process.env.ROOST_WORKER_LOG_DIR = priorLogDir;
    try { rmSync(dumpRoot, { recursive: true, force: true }); } catch { /* ignore */ }
  });

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
  });

  test("dump returns null on empty ring", () => {
    expect(bc.dump("sid-never-pushed", "noop")).toBeNull();
  });

  test.skipIf(SKIP_ON_WINDOWS)("raw PTY dumps land owner-only under a default umask", () => {
    const sid = "sid-test-mode";
    bc.push(sid, new Uint8Array([0x73, 0x65, 0x63]), 3);
    const path = bc.dump(sid, "mode_new_dir");
    expect(path).not.toBeNull();
    expect(statSync(dumpDir).mode & 0o777).toBe(0o700);
    expect(statSync(path!).mode & 0o777).toBe(0o600);
  });

  test.skipIf(SKIP_ON_WINDOWS)("dump tightens a dump dir that already existed world-readable", () => {
    mkdirSync(dumpDir, { recursive: true });
    chmodSync(dumpDir, 0o755);
    expect(statSync(dumpDir).mode & 0o777).toBe(0o755);

    const sid = "sid-test-mode-existing";
    bc.push(sid, new Uint8Array([0x73, 0x65, 0x63]), 3);
    const path = bc.dump(sid, "mode_existing_dir");
    expect(path).not.toBeNull();
    expect(statSync(dumpDir).mode & 0o777).toBe(0o700);
    expect(statSync(path!).mode & 0o777).toBe(0o600);
  });
});
