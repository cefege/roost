// Staging buffer for session-resume(): callbacks that hold live keeper output
// while the adoption window rebuilds a core, then replay it once the real
// SessionRecord exists. Owned by resume(); overflow policy lives in
// RESUME_STAGE_CAP_BYTES (session-constants.ts) and is reported through the
// shared `ResumeStageState.overflowed` flag — never thrown from onOutput,
// because these callbacks run on the pool socket's data handler where an
// exception would kill the connection instead of failing adoption cleanly.

import type { MuxChannelCallbacks } from "./keeper/multiplexed-client.ts";
import { RESUME_STAGE_CAP_BYTES } from "./session-constants.ts";
import { log } from "@roost/shared/log";

export type PendingResumeEvent =
	| { readonly kind: "output"; readonly chunk: Buffer }
	| { readonly kind: "exit"; readonly exitCode: number | null }
	| { readonly kind: "error"; readonly error: Error };

/** Shared between stageResumeCallbacks and resume()'s commit check. */
export interface ResumeStageState {
	overflowed: boolean;
}

export function stageResumeCallbacks(
	live: MuxChannelCallbacks,
	pending: PendingResumeEvent[],
	stage: ResumeStageState,
): MuxChannelCallbacks {
	let stagedBytes = 0;
	return {
		onOutput: (chunk) => {
			// Post-cap bytes are dropped without buffering: stream integrity is
			// already lost, and resume() throws at its commit check so the whole
			// adoption downgrades into the respawn path.
			if (stage.overflowed) return;
			stagedBytes += chunk.byteLength;
			if (stagedBytes > RESUME_STAGE_CAP_BYTES) {
				stage.overflowed = true;
				pending.length = 0;
				log.warn("session-manager", "resume_stage_overflow", {
					bytes: stagedBytes,
					cap_bytes: RESUME_STAGE_CAP_BYTES,
				});
				return;
			}
			pending.push({ kind: "output", chunk: Buffer.from(chunk) });
		},
		onExit: (exitCode) => {
			if (stage.overflowed) return;
			pending.push({ kind: "exit", exitCode });
		},
		onError: (error) => {
			if (stage.overflowed) return;
			pending.push({ kind: "error", error });
		},
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
