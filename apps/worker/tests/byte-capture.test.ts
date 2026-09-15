// byte-capture ring + capture-storage tests. Verify:
//   - push appends + caps at 256KB, drop clears the ring
//   - snapshotByteCapture hands back an OWNED tail with absolute bounds, and
//     that tail survives into a written incident bundle (the raw-tail
//     capability the retired dump() used to own)
//   - the capture dir is 0700 and every capture 0600, including when the dir
//     already existed with looser permissions
//   - a duplicate capture ID is refused rather than overwritten
//   - retention is COMBINED over bytecap-*.bin and terminal-incident-*.json.gz,
//     by age and by the file/byte caps
//   - an unwritable directory reports storage_failed and nothing else
// The capture directory is redirected per test so no case touches the real
// worker log dir or changes its permissions.

import { describe, expect, test, beforeEach, afterEach } from "bun:test";
import {
	chmodSync,
	mkdirSync,
	mkdtempSync,
	readdirSync,
	readFileSync,
	rmSync,
	statSync,
	truncateSync,
	utimesSync,
	writeFileSync,
} from "node:fs";
import { gunzipSync } from "node:zlib";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { TERMINAL_CAPTURE_LIMITS, terminalCaptureFileName } from "@roost/shared/terminal-capture";
import * as bc from "../src/diag/byte-capture.ts";
import {
	sweepCaptureRetention,
	writeTerminalIncidentFile,
	_resetCaptureStorageForTest,
} from "../src/diag/capture-storage.ts";
import { writeTerminalIncidentBundle } from "../src/diag/terminal-capture-bundle-writer.ts";
import { workerSectionFixture, incidentBundleInputFixture } from "./terminal-capture-fixtures.ts";

const RING_CAP = 256 * 1024;
const SKIP_ON_WINDOWS = process.platform === "win32";
const CAPTURE_A = "aaaaaaaa-0000-4000-8000-00000000aaaa";
const CAPTURE_B = "bbbbbbbb-0000-4000-8000-00000000bbbb";

let captureRoot: string;
let captureDir: string;
const priorLogDir = process.env.ROOST_WORKER_LOG_DIR;

describe("byte-capture", () => {
	beforeEach(() => {
		bc._resetForTest();
		_resetCaptureStorageForTest();
		captureRoot = mkdtempSync(join(tmpdir(), "roost-bytecap-"));
		// Nested so the capture dir itself does not exist yet: mkdtemp's own 0700
		// would make the created-by-us mode assertion vacuous.
		captureDir = join(captureRoot, "RoostWorker");
		process.env.ROOST_WORKER_LOG_DIR = captureDir;
	});

	afterEach(() => {
		_resetCaptureStorageForTest();
		if (priorLogDir === undefined) delete process.env.ROOST_WORKER_LOG_DIR;
		else process.env.ROOST_WORKER_LOG_DIR = priorLogDir;
		try { rmSync(captureRoot, { recursive: true, force: true }); } catch { /* ignore */ }
	});

	test("push appends + caps at RING_CAP_BYTES", () => {
		const sid = "sid-test-cap";
		// Push 100KB twice — total 200KB, under the cap, full retention.
		bc.push(sid, new Uint8Array(100_000).fill(0xAA), 100_000);
		bc.push(sid, new Uint8Array(100_000).fill(0xBB), 200_000);
		const tail = bc.snapshotByteCapture(sid)!;
		expect(tail.byte_length).toBe(200_000);
		expect(tail.end_offset).toBe("200000");
		expect(tail.start_offset).toBe("0");

		// Push enough to exceed the cap. Verify the ring keeps only the tail and
		// the reported start offset moves with the eviction.
		bc.push(sid, new Uint8Array(200_000).fill(0xCC), 400_000);
		const capped = bc.snapshotByteCapture(sid)!;
		expect(capped.byte_length).toBe(RING_CAP);
		expect(capped.end_offset).toBe("400000");
		expect(capped.start_offset).toBe(String(400_000 - RING_CAP));
		const bytes = Buffer.from(capped.base64, "base64");
		expect(bytes[0]).toBe(0xBB);
		expect(bytes[bytes.length - 1]).toBe(0xCC);
	});

	test("drop clears the ring", () => {
		const sid = "sid-test-drop";
		bc.push(sid, new Uint8Array(1000), 1000);
		expect(bc.snapshotByteCapture(sid)).not.toBeNull();
		bc.drop(sid);
		expect(bc.snapshotByteCapture(sid)).toBeNull();
	});

	test("snapshotByteCapture returns null for an unknown or empty ring", () => {
		expect(bc.snapshotByteCapture("sid-never-pushed")).toBeNull();
	});

	test("the retained raw tail survives into a written incident bundle", async () => {
		const sid = "sid-test-bundle";
		bc.push(sid, new Uint8Array([0x01, 0x02, 0x03, 0x04, 0x05]), 5);
		const tail = bc.snapshotByteCapture(sid)!;
		const written = await writeTerminalIncidentBundle(
			incidentBundleInputFixture(CAPTURE_A, workerSectionFixture({ byte_capture: tail })),
		);
		expect(written.ok).toBe(true);
		if (!written.ok) return;
		const bundle = JSON.parse(gunzipSync(readFileSync(written.path)).toString("utf8"));
		expect(bundle.worker.byte_capture).toEqual({
			end_offset: "5",
			start_offset: "0",
			byte_length: 5,
			base64: Buffer.from([1, 2, 3, 4, 5]).toString("base64"),
		});
	});

	test.skipIf(SKIP_ON_WINDOWS)("raw captures land owner-only under a default umask", () => {
		const written = writeTerminalIncidentFile(CAPTURE_A, new Uint8Array([0x73, 0x65, 0x63]));
		expect(written.ok).toBe(true);
		if (!written.ok) return;
		expect(statSync(captureDir).mode & 0o777).toBe(0o700);
		expect(statSync(written.path).mode & 0o777).toBe(0o600);
	});

	test.skipIf(SKIP_ON_WINDOWS)("a capture tightens a dir that already existed world-readable", () => {
		mkdirSync(captureDir, { recursive: true });
		chmodSync(captureDir, 0o755);
		expect(statSync(captureDir).mode & 0o777).toBe(0o755);

		const written = writeTerminalIncidentFile(CAPTURE_B, new Uint8Array([0x73, 0x65, 0x63]));
		expect(written.ok).toBe(true);
		if (!written.ok) return;
		expect(statSync(captureDir).mode & 0o777).toBe(0o700);
		expect(statSync(written.path).mode & 0o777).toBe(0o600);
	});

	test("a duplicate capture ID is refused, never overwritten", () => {
		const first = writeTerminalIncidentFile(CAPTURE_A, new Uint8Array([0x01]));
		expect(first.ok).toBe(true);
		const retry = writeTerminalIncidentFile(CAPTURE_A, new Uint8Array([0x02, 0x02]));
		expect(retry).toEqual({ ok: false, code: "storage_failed" });
		if (!first.ok) return;
		expect(Array.from(readFileSync(first.path))).toEqual([0x01]);
	});

	test("an unwritable capture directory reports storage_failed", () => {
		// A regular file where the directory should be: mkdir and write both fail.
		writeFileSync(join(captureRoot, "blocker"), "x");
		process.env.ROOST_WORKER_LOG_DIR = join(captureRoot, "blocker", "nested");
		expect(writeTerminalIncidentFile(CAPTURE_A, new Uint8Array([0x01])))
			.toEqual({ ok: false, code: "storage_failed" });
	});

	test("retention removes both capture families once past the window", () => {
		mkdirSync(captureDir, { recursive: true });
		const stale = [
			join(captureDir, "bytecap-old-1.bin"),
			join(captureDir, terminalCaptureFileName(CAPTURE_A)),
		];
		const fresh = [
			join(captureDir, "bytecap-new-1.bin"),
			join(captureDir, terminalCaptureFileName(CAPTURE_B)),
		];
		const neighbour = join(captureDir, "keeper.err.log");
		for (const path of [...stale, ...fresh, neighbour]) writeFileSync(path, "x");
		const staleSeconds = (Date.now() - TERMINAL_CAPTURE_LIMITS.retentionMs - 60_000) / 1000;
		for (const path of [...stale, neighbour]) utimesSync(path, staleSeconds, staleSeconds);

		sweepCaptureRetention();

		const remaining = readdirSync(captureDir).sort();
		// The stale neighbour log is older than every capture and still present:
		// deletion is restricted to names this owner created.
		expect(remaining).toEqual([
			"bytecap-new-1.bin",
			"keeper.err.log",
			terminalCaptureFileName(CAPTURE_B),
		].sort());
	});

	test("the combined file cap evicts oldest-first across both families", () => {
		mkdirSync(captureDir, { recursive: true });
		const names: string[] = [];
		for (let idx = 0; idx < TERMINAL_CAPTURE_LIMITS.storageFiles + 2; idx += 1) {
			const name = idx % 2 === 0
				? `bytecap-sid-${idx}.bin`
				: `terminal-incident-00000000-0000-4000-8000-${String(idx).padStart(12, "0")}.json.gz`;
			names.push(name);
			const path = join(captureDir, name);
			writeFileSync(path, "x");
			const seconds = (Date.now() - (names.length * 10_000)) / 1000;
			utimesSync(path, seconds, seconds);
		}

		// The PERIODIC sweep holds the cap exactly; it must not evict a file to
		// make room for a write nobody issued.
		sweepCaptureRetention();
		const afterPeriodic = new Set(readdirSync(captureDir));
		expect(afterPeriodic.size).toBe(TERMINAL_CAPTURE_LIMITS.storageFiles);
		// `names` was written newest-first by mtime, so the tail entries are the
		// oldest and are exactly what a combined LRU must drop.
		expect(afterPeriodic.has(names[0]!)).toBe(true);
		expect(afterPeriodic.has(names[names.length - 1]!)).toBe(false);

		// The WRITE path reserves one slot so the file it is about to store
		// cannot push the directory past the cap.
		const written = writeTerminalIncidentFile(CAPTURE_A, new Uint8Array([0x01]));
		expect(written.ok).toBe(true);
		expect(readdirSync(captureDir)).toHaveLength(TERMINAL_CAPTURE_LIMITS.storageFiles);
	});

	test("the combined byte cap evicts even when the file count is fine", () => {
		mkdirSync(captureDir, { recursive: true });
		const huge = join(captureDir, "bytecap-huge.bin");
		writeFileSync(huge, "x");
		// Sparse: apparent size crosses the cap without consuming the disk.
		truncateSync(huge, TERMINAL_CAPTURE_LIMITS.storageBytes);
		const small = join(captureDir, terminalCaptureFileName(CAPTURE_B));
		writeFileSync(small, "x");
		const older = (Date.now() - 60_000) / 1000;
		utimesSync(huge, older, older);

		sweepCaptureRetention();

		expect(readdirSync(captureDir)).toEqual([terminalCaptureFileName(CAPTURE_B)]);
	});
});
