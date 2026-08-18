// Browser-command handlers: diagnostics (diag-dump-bytecap /
// diag-snapshot). Extracted from browser-command-handler.ts (CLAUDE.md 400-line cap).

import type { ClientControlFrame } from "@roost/shared/wire";
import type { CoordLink } from "./transport/CoordLink.ts";
import type { SessionManager } from "./session-manager.ts";

export function handleDiagDumpBytecap(
	frame: Extract<ClientControlFrame, { kind: "diag-dump-bytecap" }>,
	request_id: string,
	deps: { coordLink: CoordLink },
): void {
	const { coordLink } = deps;
	// diag — coord asks for an on-disk dump of this session's byte ring
	void import("./diag/byte-capture.ts").then((bc) => {
		const path = bc.dump(frame.session_id, frame.reason);
		coordLink.send({ kind: "rpc-ok", request_id, data: { path } });
	});
	return;
}

export function handleDiagSnapshot(
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): void {
	const { coordLink, sessionMgr } = deps;
	try {
		const data = sessionMgr.diagSnapshot();
		coordLink.send({ kind: "rpc-ok", request_id, data });
	} catch (error) {
		coordLink.send({
			kind: "rpc-error",
			request_id,
			message: (error instanceof Error ? error.message : String(error)).slice(0, 200),
		});
	}
	return;
}
