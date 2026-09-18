// The one classifier for "is this machine on the fleet's current release?".
// The desired fleet SHA is the COORDINATOR's own SHA: `roost push` activates the
// coordinator at the target commit before it converges any worker, so a worker
// behind the coordinator is definitionally behind the fleet — no second record
// of intent can drift out of sync with the running coordinator.
// Callers: roost-cli status output, web MachineCard, coord catch-up admission.
// Depends on: nothing.

export type WorkerUpdateState =
	| "unknown"
	| "up-to-date"
	| "updating"
	| "update-available"
	| "update-deferred";

export interface WorkerUpdateInputs {
	/** The SHA the worker last reported through its heartbeat. */
	readonly workerGitSha: string | null;
	/** The running coordinator's SHA — the fleet's desired release. */
	readonly coordGitSha: string | null;
	/** Is the worker reachable right now (not merely heartbeat-fresh)? */
	readonly online: boolean;
	/** Is a deploy to this worker's host in flight? */
	readonly deployInFlight: boolean;
}

/** `update-deferred` is the state this whole model exists for: the machine is
 *  behind and unreachable, so it is skipped now and updated when it returns —
 *  never a reason to refuse the rest of the fleet. */
export function workerUpdateState(inputs: WorkerUpdateInputs): WorkerUpdateState {
	if (inputs.deployInFlight) return "updating";
	if (!inputs.workerGitSha || !inputs.coordGitSha) return "unknown";
	if (inputs.workerGitSha === inputs.coordGitSha) return "up-to-date";
	return inputs.online ? "update-available" : "update-deferred";
}

/** One wording per state, so the CLI row and the web badge cannot diverge. */
export const WORKER_UPDATE_LABELS: Readonly<Record<WorkerUpdateState, string>> = {
	unknown: "Version unknown",
	"up-to-date": "Up to date",
	updating: "Updating…",
	"update-available": "Update available",
	"update-deferred": "Update pending — offline",
};
