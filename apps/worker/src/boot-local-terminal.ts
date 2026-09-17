// Boot-time assembly of the local terminal fast path: the terminal-view owner
// this worker uses for its own sessions, the in-memory grant store, the
// loopback socket handlers, and the local UI door that serves the SPA.
// runWorker() builds this before the coordinator link so an already-granted
// browser keeps a door to its own PTYs while the link is down. The session
// manager and the link are reached through the same forward refs the
// coord-link callbacks use.

import { createSpaResponder } from "@roost/shared/spa";
import { WEB_ASSETS } from "@roost/shared/web-embed";
import type { WorkerFp } from "@roost/shared/wire";
import type { CoordLinkRefs, LocalTerminalWiring } from "./coord-link-deps.ts";
import { LocalTerminalGrantStore } from "./local-terminal-grants.ts";
import { LocalTerminalSockets } from "./local-terminal-socket.ts";
import { startLocalUiServer } from "./local-ui-server.ts";
import type { SessionManager } from "./session-manager.ts";
import { TerminalViewOwner } from "./terminal-view-owner.ts";

export interface LocalTerminalDoorOptions {
	bind: string;
	coordinatorUrl: string;
	workerFp: WorkerFp;
	allowedBrowserOrigins: readonly string[];
	webDistPath: string | undefined;
	refs: CoordLinkRefs;
}

export interface LocalTerminalDoor {
	wiring: LocalTerminalWiring;
	close(): void;
}

export function startLocalTerminalDoor(options: LocalTerminalDoorOptions): LocalTerminalDoor {
	const { refs } = options;
	// A local socket cannot arrive before its grant, and a grant is only
	// installed over an established coordinator link, so an unbound read here is
	// a boot-wiring bug rather than a race.
	const sessions = (): SessionManager => {
		if (!refs.sessionMgr) throw new Error("local terminal door used before sessionMgr was bound");
		return refs.sessionMgr;
	};
	const viewOwner = new TerminalViewOwner({
		sessions,
		sendViewState: (socketId, frame) => { refs.link?.sendTerminalViewState(socketId, frame); },
		sendProjection: (projection) => { refs.link?.sendTerminalViewProjection(projection); },
	});
	const grants = new LocalTerminalGrantStore();
	const sockets = new LocalTerminalSockets({
		sessions,
		grants,
		viewOwner,
		workerFingerprint: options.workerFp,
	});
	const server = startLocalUiServer({
		bind: options.bind,
		coordinatorUrl: options.coordinatorUrl,
		workerFingerprint: options.workerFp,
		allowedBrowserOrigins: options.allowedBrowserOrigins,
		spa: createSpaResponder(options.webDistPath, WEB_ASSETS),
		terminal: sockets,
	});
	return {
		wiring: { viewOwner, grants, sockets },
		close: () => {
			server.close();
			viewOwner.dispose();
		},
	};
}
