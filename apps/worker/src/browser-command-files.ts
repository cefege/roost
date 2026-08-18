// Browser-command handlers: filesystem RPCs (read-file / list-dir / mkdir /
// get-home). Extracted from browser-command-handler.ts (CLAUDE.md 400-line cap).

import type { ClientControlFrame } from "@roost/shared/wire";
import type { CoordLink } from "./transport/coord-link.ts";

export function handleReadFile(
	frame: Extract<ClientControlFrame, { kind: "read-file" }>,
	request_id: string,
	deps: { coordLink: CoordLink },
): void {
	const { coordLink } = deps;
	void import("./file-rpcs.ts").then(({ readFileRpc }) =>
		readFileRpc(frame.path).then((reply) =>
			coordLink.send(
				reply.kind === "rpc-ok"
					? { kind: "rpc-ok", request_id, data: reply.data }
					: { kind: "rpc-error", request_id, message: reply.message },
			),
		),
	);
	return;
}

export function handleReadFileChunk(
	frame: Extract<ClientControlFrame, { kind: "read-file-chunk" }>,
	request_id: string,
	deps: { coordLink: CoordLink },
): void {
	const { coordLink } = deps;
	void import("./file-rpcs.ts").then(({ readFileChunkRpc }) =>
		readFileChunkRpc(frame.path, frame.offset, frame.len).then((reply) =>
			coordLink.send(
				reply.kind === "rpc-ok"
					? { kind: "rpc-ok", request_id, data: reply.data }
					: { kind: "rpc-error", request_id, message: reply.message },
			),
		),
	);
	return;
}

export function handleListDir(
	frame: Extract<ClientControlFrame, { kind: "list-dir" }>,
	request_id: string,
	deps: { coordLink: CoordLink },
): void {
	const { coordLink } = deps;
	void import("./file-rpcs.ts").then(({ listDirRpc }) =>
		listDirRpc(frame.path).then((reply) =>
			coordLink.send(
				reply.kind === "rpc-ok"
					? { kind: "rpc-ok", request_id, data: reply.data }
					: { kind: "rpc-error", request_id, message: reply.message },
			),
		),
	);
	return;
}

export function handleMkdir(
	frame: Extract<ClientControlFrame, { kind: "mkdir" }>,
	request_id: string,
	deps: { coordLink: CoordLink },
): void {
	const { coordLink } = deps;
	void import("./file-rpcs.ts").then(({ mkdirRpc }) =>
		mkdirRpc(frame.path).then((reply) =>
			coordLink.send(
				reply.kind === "rpc-ok"
					? { kind: "rpc-ok", request_id, data: reply.data }
					: { kind: "rpc-error", request_id, message: reply.message },
			),
		),
	);
	return;
}

export function handleGetHome(
	request_id: string,
	deps: { coordLink: CoordLink },
): void {
	const { coordLink } = deps;
	coordLink.send({
		kind: "rpc-ok",
		request_id,
		data: { home: process.env.HOME ?? "~" },
	});
	return;
}
