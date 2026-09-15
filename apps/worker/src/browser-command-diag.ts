// Browser-command handler: the ordinary content-free diag snapshot.
// Called by browser-command-handler.ts; answers upstream as rpc-ok / rpc-error.
// Opt-in terminal incident capture is a separate command with its own handler
// in browser-command-terminal-capture.ts.

import type { CoordLink } from "./transport/coord-link.ts";
import type { SessionManager } from "./session-manager.ts";

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
