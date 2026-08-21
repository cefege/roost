import type { MuxChannelCallbacks } from "./keeper/multiplexed-client.ts";

export type PendingResumeEvent =
	| { readonly kind: "output"; readonly chunk: Buffer }
	| { readonly kind: "exit"; readonly exitCode: number | null }
	| { readonly kind: "error"; readonly error: Error };

export function stageResumeCallbacks(
	live: MuxChannelCallbacks,
	pending: PendingResumeEvent[],
): MuxChannelCallbacks {
	return {
		onOutput: (chunk) =>
			pending.push({ kind: "output", chunk: Buffer.from(chunk) }),
		onExit: (exitCode) => pending.push({ kind: "exit", exitCode }),
		onError: (error) => pending.push({ kind: "error", error }),
	};
}

export function flushResumeEvents(
	live: MuxChannelCallbacks,
	pending: PendingResumeEvent[],
): void {
	for (const event of pending) {
		if (event.kind === "output") live.onOutput(event.chunk);
		else if (event.kind === "exit") live.onExit(event.exitCode);
		else live.onError(event.error);
	}
	pending.length = 0;
}
