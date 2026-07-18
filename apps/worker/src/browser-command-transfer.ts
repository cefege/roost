// Browser-command handler: cross-worker rsync transfer (start-transfer).
// Extracted from browser-command-handler.ts (CLAUDE.md 400-line cap).

import type { ClientControlFrame } from "@roost/shared/wire";
import type { CoordLink } from "./transport/CoordLink.ts";

export function handleStartTransfer(
	frame: Extract<ClientControlFrame, { kind: "start-transfer" }>,
	deps: { coordLink: CoordLink },
): void {
	const { coordLink } = deps;
	// Cross-worker rsync. Stream every output line back to coord
	// via transfer-line frames; emit transfer-done on exit.
	const jobId = frame.job_id;
	const cmd: string[] = [
		"rsync",
		"-azP",
		"--info=progress2",
		...(frame.delete_extra ? ["--delete"] : []),
		...(frame.dry_run ? ["--dry-run"] : []),
		"-e",
		"ssh -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10",
		frame.src_path,
		`${frame.dst_host}:${frame.dst_path}`,
	];
	coordLink.send({
		kind: "transfer-line",
		job_id: jobId,
		text: `>> ${cmd.join(" ")}`,
	});
	try {
		const proc = Bun.spawn({ cmd, stdout: "pipe", stderr: "pipe" });
		async function pump(stream: ReadableStream<Uint8Array>): Promise<void> {
			const reader = stream.getReader();
			const dec = new TextDecoder();
			let buf = "";
			while (true) {
				const { value, done } = await reader.read();
				if (done) break;
				buf += dec.decode(value, { stream: true });
				let idx: number;
				// rsync emits \r for progress refresh — split on it too.
				while ((idx = buf.search(/[\r\n]/)) !== -1) {
					const line = buf.slice(0, idx).trim();
					buf = buf.slice(idx + 1);
					if (line.length > 0)
						coordLink.send({
							kind: "transfer-line",
							job_id: jobId,
							text: line,
						});
				}
			}
			if (buf.trim().length > 0)
				coordLink.send({
					kind: "transfer-line",
					job_id: jobId,
					text: buf.trim(),
				});
		}
		void Promise.all([pump(proc.stdout), pump(proc.stderr), proc.exited])
			.then(([, , exit]) => {
				coordLink.send({
					kind: "transfer-done",
					job_id: jobId,
					exit: exit ?? null,
				});
			})
			.catch((e) => {
				coordLink.send({
					kind: "transfer-done",
					job_id: jobId,
					exit: null,
					error: (e as Error).message,
				});
			});
	} catch (e) {
		coordLink.send({
			kind: "transfer-done",
			job_id: jobId,
			exit: null,
			error: (e as Error).message,
		});
	}
	return;
}
