// Shared delivery transport for the agent-status integrations (omp + pi).
// Both integrations post {version, capability, method:"agent.report", params}
// frames over the local report-server socket with a 500ms→1500ms two-attempt
// ladder and a latest-report-wins drain loop; these were two byte-identical
// copies until pi's froze with @ts-nocheck while omp evolved.
//
// DEPLOYMENT CONSTRAINT: the integration sources are shipped VERBATIM as
// standalone extension files into user config dirs (install-integrations.ts /
// gen-embed.ts), where "@roost/*" imports cannot resolve. In-repo they import
// THIS module normally; standalone-integration.ts splices this file's source
// text in place of that import at embed/install time, so keep this module
// self-contained (node builtins only, no @roost imports).

import net from "node:net";

export type AgentReportState = "working" | "blocked" | "idle";

export interface QueuedAgentReport {
	state: AgentReportState;
	message?: string;
	active: boolean;
	seq: number;
}

export interface AgentReporterConfig {
	/** Value of params.agent on every frame ("omp" / "pi"). */
	agent: string;
	endpoint: string;
	capability: string;
	sessionId: string;
}

/** Build the fire-and-forget reporter an integration publishes state through.
 *  Every call queues the newest report (seq-monotonic) and drains in the
 *  background: delivery attempts never block the host agent, a failed attempt
 *  is retried once at the longer timeout, and only the LAST queued state ever
 *  matters because the server keeps no history. */
export function createAgentReporter(
	config: AgentReporterConfig,
): ((state: AgentReportState, message?: string, active?: boolean) => void) {
	let reportSeq = Date.now() * 1_000;
	let queuedReport: QueuedAgentReport | undefined;
	let sendInFlight = false;

	const sendAttempt = (report: QueuedAgentReport, timeoutMs: number): Promise<boolean> => {
		const request = {
			version: 1,
			capability: config.capability,
			method: "agent.report",
			params: {
				session_id: config.sessionId,
				pid: process.pid,
				agent: config.agent,
				state: report.state,
				message: report.message,
				seq: report.seq,
				active: report.active,
			},
		};
		const { promise, resolve } = Promise.withResolvers<boolean>();
		const socket = net.createConnection(config.endpoint);
		let settled = false;
		let timer: ReturnType<typeof setTimeout> | undefined;
		const finish = (delivered: boolean) => {
			if (settled) return;
			settled = true;
			clearTimeout(timer);
			socket.destroy();
			resolve(delivered);
		};
		socket.on("error", () => finish(false));
		socket.on("connect", () => socket.write(`${JSON.stringify(request)}\n`));
		socket.on("data", () => finish(true));
		socket.on("end", () => finish(false));
		timer = setTimeout(() => finish(false), timeoutMs);
		timer.unref?.();
		return promise;
	};
	const sendNow = async (report: QueuedAgentReport): Promise<void> => {
		if (await sendAttempt(report, 500)) return;
		await sendAttempt(report, 1_500);
	};
	const drain = async (): Promise<void> => {
		if (sendInFlight) return;
		sendInFlight = true;
		try {
			while (queuedReport) {
				const report = queuedReport;
				queuedReport = undefined;
				await sendNow(report);
			}
		} finally {
			sendInFlight = false;
			if (queuedReport) void drain();
		}
	};
	return (state, message, active = true) => {
		queuedReport = { state, message, active, seq: ++reportSeq };
		if (!sendInFlight) void drain();
	};
}
