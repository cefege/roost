// The ONE owner of terminal-capture file storage on a worker host: the
// owner-only log directory, exclusive 0600 writes named by capture UUID, and
// the COMBINED retention sweep over legacy byte-ring dumps and incident
// bundles. Called by diag/terminal-capture-bundle-writer.ts (write) and
// session-lifecycle.ts (shutdown). Directory comes from @roost/shared/paths;
// every bound and every filename comes from @roost/shared/terminal-capture.

import {
	chmodSync,
	mkdirSync,
	readdirSync,
	statSync,
	unlinkSync,
	writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { diag } from "@roost/shared/diag";
import { workerLogDir } from "@roost/shared/paths";
import {
	isTerminalCaptureFileName,
	TERMINAL_CAPTURE_LIMITS,
	terminalCaptureFileName,
	type TerminalCaptureErrorCode,
} from "@roost/shared/terminal-capture";

const CAPTURE_DIR_MODE = 0o700;
const CAPTURE_FILE_MODE = 0o600;
/** Retention is a 24 h ceiling, so an hourly sweep is fine-grained enough and
 *  costs one readdir of a directory this owner alone populates. */
const RETENTION_SWEEP_INTERVAL_MS = 60 * 60_000;
const LEGACY_BYTECAP_PREFIX = "bytecap-";
const LEGACY_BYTECAP_SUFFIX = ".bin";

export type CaptureWriteOutcome =
	| { readonly ok: true; readonly path: string; readonly byte_length: number }
	| { readonly ok: false; readonly code: TerminalCaptureErrorCode };

interface StoredCaptureFile {
	readonly path: string;
	readonly mtimeMs: number;
	readonly size: number;
}

let _ownerOnlyCaptureDir: string | null = null;
let _retentionTimer: NodeJS.Timeout | null = null;

/** True for a name this owner created. Deletion is restricted to recognized
 *  capture files so retention can never unlink a neighbour's log — the
 *  directory also holds keeper.err.log and the worker's own output. */
function isRecognizedCaptureFileName(name: string): boolean {
	if (isTerminalCaptureFileName(name)) return true;
	return name.startsWith(LEGACY_BYTECAP_PREFIX) && name.endsWith(LEGACY_BYTECAP_SUFFIX);
}

/** Write one incident bundle. The capture UUID is the whole filename and the
 *  create is EXCLUSIVE: an RPC retry that reached the writer twice must fail
 *  rather than overwrite the evidence the first attempt already froze. */
export function writeTerminalIncidentFile(
	captureId: string,
	payload: Uint8Array,
): CaptureWriteOutcome {
	const dir = workerLogDir();
	ensureOwnerOnlyCaptureDir(dir);
	// Sweep with this file's slot and bytes RESERVED, so it cannot push the
	// combined footprint past either cap for the duration of its life.
	sweepCaptureRetention({ reserveSlot: true, reserveBytes: payload.byteLength });
	ensureCaptureRetention();
	const path = join(dir, terminalCaptureFileName(captureId));
	try {
		writeFileSync(path, payload, { mode: CAPTURE_FILE_MODE, flag: "wx" });
	} catch {
		// Only the capture ID and a fixed code may leave here: the payload is
		// terminal content and an errno message can carry a path a caller chose.
		diag("diag.terminal_capture_write_failed", {
			capture_id: captureId,
			byte_len: payload.byteLength,
		});
		return { ok: false, code: "storage_failed" };
	}
	diag("diag.terminal_capture_written", {
		capture_id: captureId,
		byte_len: payload.byteLength,
	});
	return { ok: true, path, byte_length: payload.byteLength };
}

/** Run the startup sweep and arm the periodic one. Idempotent; the timer is
 *  unref'd so it never holds the process open. */
export function ensureCaptureRetention(): void {
	if (_retentionTimer !== null) return;
	sweepCaptureRetention();
	const timer = setInterval(() => { sweepCaptureRetention(); }, RETENTION_SWEEP_INTERVAL_MS);
	timer.unref?.();
	_retentionTimer = timer;
}

export function stopCaptureRetention(): void {
	if (_retentionTimer === null) return;
	clearInterval(_retentionTimer);
	_retentionTimer = null;
}

/** Remove recognized capture files past the retention window, then enforce the
 *  combined file/byte caps oldest-first. Both dump families share one budget:
 *  two independent LRUs would each believe it had the whole disk.
 *
 *  `reserveSlot` makes room for ONE more file and belongs only to the write
 *  path: a periodic sweep that reserved a slot would evict the oldest bundle
 *  every hour from a steady state at exactly the cap, with nothing to store. */
export function sweepCaptureRetention(
	options: {
		readonly reserveSlot?: boolean;
		readonly reserveBytes?: number;
	} = {},
): void {
	const dir = workerLogDir();
	let stored: StoredCaptureFile[];
	try {
		stored = readdirSync(dir)
			.filter(isRecognizedCaptureFileName)
			.map((name) => {
				const path = join(dir, name);
				const stats = statSync(path);
				return { path, mtimeMs: stats.mtimeMs, size: stats.size };
			});
	} catch { return; }
	const expiredBefore = Date.now() - TERMINAL_CAPTURE_LIMITS.retentionMs;
	const retained: StoredCaptureFile[] = [];
	for (const file of stored) {
		if (file.mtimeMs < expiredBefore) {
			if (removeCaptureFile(file.path)) continue;
		}
		retained.push(file);
	}
	retained.sort((left, right) => left.mtimeMs - right.mtimeMs);
	let totalBytes = retained.reduce((acc, file) => acc + file.size, 0);
	const fileCap = TERMINAL_CAPTURE_LIMITS.storageFiles - (options.reserveSlot ? 1 : 0);
	const byteCap = TERMINAL_CAPTURE_LIMITS.storageBytes - (options.reserveBytes ?? 0);
	while (
		retained.length > 0
		&& (retained.length > fileCap || totalBytes > byteCap)
	) {
		const victim = retained.shift();
		if (!victim) break;
		if (removeCaptureFile(victim.path)) totalBytes -= victim.size;
	}
}

/** Test seam: forget the tightened-directory memo and stop the sweep so a
 *  redirected log dir is re-checked from scratch. */
export function _resetCaptureStorageForTest(): void {
	_ownerOnlyCaptureDir = null;
	stopCaptureRetention();
}

/** Create the capture directory owner-only, and tighten a directory that a
 *  looser umask (or an older worker) already created — `mkdirSync`'s mode is
 *  ignored on an existing path, so raw PTY bytes would land in a 0755 dir. */
function ensureOwnerOnlyCaptureDir(dir: string): void {
	try {
		mkdirSync(dir, { recursive: true, mode: CAPTURE_DIR_MODE });
	} catch { /* directory exists or unwritable; the write below reports it */ }
	if (_ownerOnlyCaptureDir === dir || process.platform === "win32") return;
	try {
		const mode = statSync(dir).mode & 0o777;
		if (mode !== CAPTURE_DIR_MODE) {
			chmodSync(dir, CAPTURE_DIR_MODE);
			diag("diag.capture_dir_tightened", { dir, from_mode: mode.toString(8) });
		}
		_ownerOnlyCaptureDir = dir;
	} catch { /* not ours or gone; the write below reports the real failure */ }
}

function removeCaptureFile(path: string): boolean {
	try {
		unlinkSync(path);
		return true;
	} catch { return false; }
}
