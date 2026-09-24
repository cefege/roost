// Windows-only durable update progress replay for CoordLink snapshot recovery.
// Static import would load the Windows CLI module on POSIX startup; this
// platform-gated dynamic import keeps that host-only dependency conditional.
// Replay uses ordinary upstream progress frames.
import { randomUUID } from "node:crypto";
import type { CoordLink } from "./transport/coord-link.ts";

export async function replayDurableWindowsUpdateProgress(coordLink: CoordLink): Promise<void> {
	if (process.platform !== "win32") return;
	const {
		DurableWindowsUpdateJournalStore,
		readWindowsUpdateProgressFromJournal,
	} = await import("@roost/host/windows/windows-update-journal");
	const journal = await new DurableWindowsUpdateJournalStore().load();
	if (!journal) return;
	const requestId = randomUUID();
	for (const entry of readWindowsUpdateProgressFromJournal(journal, 0)) {
		coordLink.send({
			kind: "update-progress",
			request_id: requestId,
			job_id: journal.jobId,
			sequence: entry.sequence,
			phase: entry.phase,
			message: entry.message,
			terminal: entry.terminal,
			success: entry.success,
			error: entry.error,
		});
	}
}
