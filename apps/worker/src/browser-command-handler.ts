// Browser->worker RPC dispatch. Every ClientControlFrame variant that arrives
// over CoordLink downstream (spawn / kill / scrollback / resize / transfer /
// attachments / diag / file-ops) is handled here and answered via
// coordLink.send. Extracted from main.ts (CLAUDE.md 400-line cap). Wired by
// main.ts as CoordLink.onBrowserCommand.

import { log } from "@roost/shared/log";
import type { ClientControlFrame } from "@roost/shared/wire";
import type { CoordLink } from "./transport/coord-link.ts";
import type { SessionManager } from "./session-manager.ts";
import { handleAttach, handleKill, handleRespawnIfMissing, handleSpawnShell } from "./browser-command-spawn.ts";
import { handleGetHome, handleListDir, handleMkdir, handleReadFile, handleReadFileChunk } from "./browser-command-files.ts";
import { handleGetScrollbackCells, handleSearchScrollback } from "./browser-command-terminal.ts";
import { handleStartTransfer } from "./browser-command-transfer.ts";
import { handleAttachmentProbe, handleDeleteAttachment, handleListAttachments } from "./browser-command-attachments.ts";
import { handleDiagDumpBytecap, handleDiagSnapshot } from "./browser-command-diag.ts";

export interface BrowserCommandMsg {
	browser_id: string;
	viewer_id: string;
	request_id: string;
	frame: ClientControlFrame;
}

export function handleBrowserCommand(
	msg: BrowserCommandMsg,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): void {
	const { frame, request_id } = msg;
	const { coordLink, sessionMgr } = deps;
	log.info("worker", "onBrowserCommand", { kind: frame.kind, request_id });
	switch (frame.kind) {
		case "kill": {
			handleKill(frame, request_id, { sessionMgr });
			return;
		}
		case "spawn-shell": {
			handleSpawnShell(frame, request_id, { coordLink, sessionMgr });
			return;
		}
		case "attach": {
			handleAttach(request_id, { coordLink });
			return;
		}
		case "read-file": {
			handleReadFile(frame, request_id, { coordLink });
			return;
		}
		case "read-file-chunk": {
			handleReadFileChunk(frame, request_id, { coordLink });
			return;
		}
		case "list-dir": {
			handleListDir(frame, request_id, { coordLink });
			return;
		}
		case "mkdir": {
			handleMkdir(frame, request_id, { coordLink });
			return;
		}
		case "get-home": {
			handleGetHome(request_id, { coordLink });
			return;
		}
		case "get-scrollback-cells": {
			void handleGetScrollbackCells(frame, request_id, { coordLink, sessionMgr });
			return;
		}
		case "search-scrollback": {
			void handleSearchScrollback(frame, request_id, { coordLink, sessionMgr });
			return;
		}
		case "start-transfer": {
			handleStartTransfer(frame, { coordLink });
			return;
		}
		// save-attachment (whole-file base64 frame) retired — uploads now
		// stream via DAttachmentChunk → onAttachmentChunk (attachment-upload.ts).
		case "list-attachments": {
			handleListAttachments(frame, request_id, { coordLink });
			return;
		}
		case "delete-attachment": {
			handleDeleteAttachment(frame, request_id, { coordLink });
			return;
		}
		case "attachment-probe": {
			handleAttachmentProbe(frame, request_id, { coordLink });
			return;
		}
		case "diag-dump-bytecap": {
			handleDiagDumpBytecap(frame, request_id, { coordLink });
			return;
		}
		case "diag-snapshot": {
			handleDiagSnapshot(request_id, { coordLink, sessionMgr });
			return;
		}
		case "respawn-if-missing": {
			handleRespawnIfMissing(frame, request_id, { coordLink, sessionMgr });
			return;
		}
		case "cursor-pos":
		case "set-title":
		case "detach":
		case "list-skills":
		case "git-diff": {
			// Fire-and-forget mutations or RPCs not yet ported. Logging
			// proves the wire is alive; follow-up commits port each to
			// a SessionManager method as the SPA flow needs them.
			log.debug("worker", "browser_command_via_coord", {
				kind: frame.kind,
				request_id,
			});
			return;
		}
		default: {
			const unhandled: { kind: string } = frame;
			log.warn("browser-command", "unhandled_frame", { kind: unhandled.kind });
			const _x: never = frame;
			void _x;
			return;
		}
	}
}
