// Browser-command handlers: attachment dir ops (list-attachments /
// delete-attachment). Extracted from browser-command-handler.ts (CLAUDE.md 400-line cap).

import type { ClientControlFrame } from "@roost/shared/wire";
import type { CoordLink } from "./transport/coord-link.ts";
import { normalizeWorkerPath } from "./util/path.ts";

export function handleListAttachments(
	frame: Extract<ClientControlFrame, { kind: "list-attachments" }>,
	request_id: string,
	deps: { coordLink: CoordLink },
): void {
	const { coordLink } = deps;
	// att2b — return entries (filename, size_bytes, mtime_ms,
	// abs_path) for the session's attachment dir. Newest first.
	void import("./attachment-reaper.ts").then(
		async ({ MANIFEST_NAME, resolveSessionDirWithinBase }) => {
			try {
				const path = await import("node:path");
				const fs = await import("node:fs");
				const dir = resolveSessionDirWithinBase(frame.session_id);
				if (!dir) {
					coordLink.send({
						kind: "rpc-error",
						request_id,
						message: "invalid session_id",
					});
					return;
				}
				const entries: Array<{
					filename: string;
					size_bytes: number;
					mtime_ms: number;
					abs_path: string;
				}> = [];
				if (fs.existsSync(dir)) {
					for (const fname of fs.readdirSync(dir)) {
						if (fname === MANIFEST_NAME) continue;
						try {
							const fpath = path.join(dir, fname);
							const stat = fs.statSync(fpath);
							if (!stat.isFile()) continue;
							entries.push({
								filename: fname,
								size_bytes: stat.size,
								mtime_ms: stat.mtimeMs,
								abs_path: normalizeWorkerPath(fpath),
							});
						} catch {
							/* ignore individual stat errors */
						}
					}
				}
				entries.sort((a, b) => b.mtime_ms - a.mtime_ms);
				coordLink.send({
					kind: "rpc-ok",
					request_id,
					data: { entries },
				});
			} catch (err) {
				const msg = err instanceof Error ? err.message : String(err);
				coordLink.send({ kind: "rpc-error", request_id, message: msg });
			}
		},
	);
	return;
}

export function handleDeleteAttachment(
	frame: Extract<ClientControlFrame, { kind: "delete-attachment" }>,
	request_id: string,
	deps: { coordLink: CoordLink },
): void {
	const { coordLink } = deps;
	// att2b — unlink a single file from the session's attachment dir.
	// Filename is the bare name (no path components); rejects on
	// any `/` or `..` in the input.
	void import("./attachment-reaper.ts").then(
		async ({ resolveSessionDirWithinBase }) => {
			try {
				const path = await import("node:path");
				const fs = await import("node:fs");
				if (
					frame.filename.includes("/") ||
					frame.filename.includes("\\") ||
					frame.filename === ".." ||
					frame.filename === "."
				) {
					coordLink.send({
						kind: "rpc-error",
						request_id,
						message: "invalid filename",
					});
					return;
				}
				const dir = resolveSessionDirWithinBase(frame.session_id);
				if (!dir) {
					coordLink.send({
						kind: "rpc-error",
						request_id,
						message: "invalid session_id",
					});
					return;
				}
				const fpath = path.join(dir, frame.filename);
				try {
					fs.unlinkSync(fpath);
				} catch (e) {
					if ((e as NodeJS.ErrnoException).code !== "ENOENT") throw e;
				}
				coordLink.send({
					kind: "rpc-ok",
					request_id,
					data: { ok: true },
				});
			} catch (err) {
				const msg = err instanceof Error ? err.message : String(err);
				coordLink.send({ kind: "rpc-error", request_id, message: msg });
			}
		},
	);
	return;
}

export function handleAttachmentProbe(
	frame: Extract<ClientControlFrame, { kind: "attachment-probe" }>,
	request_id: string,
	deps: { coordLink: CoordLink },
): void {
	const { coordLink } = deps;
	// att3 — content-dedup probe. Look the SHA-256 up in the session's manifest;
	// a hit returns the existing path so the SPA skips the byte upload.
	void import("./attachment-upload.ts").then(({ probeAttachment }) => {
		try {
			const r = probeAttachment(frame.session_id, frame.sha256, frame.short_path);
			coordLink.send({ kind: "rpc-ok", request_id, data: { hit: r.hit, abs_path: r.abs_path } });
		} catch (err) {
			coordLink.send({ kind: "rpc-error", request_id, message: err instanceof Error ? err.message : String(err) });
		}
	});
	return;
}
