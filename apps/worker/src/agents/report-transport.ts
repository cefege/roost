// Self-contained local transport shared by installed agent integrations.
// Volatile status coalesces and retries once; durable conversation references
// make exactly one request and require a complete explicit acknowledgement.
// Process identity and report ordering remain worker-owned at admission.
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
}

export interface AgentReporterConfig {
	endpoint: string;
	capability: string;
	sessionId: string;
}

export interface AgentReferenceReportValue {
	kind: "id" | "path";
	value: string;
}

export type AgentReferenceReportOutcome =
	| { status: "acknowledged" }
	| { status: "rejected"; error: string }
	| { status: "ambiguous"; reason: "disconnect" | "timeout" | "invalid_response" };

const REFERENCE_REPORT_TIMEOUT_MS = 2_000;
const REFERENCE_RESPONSE_MAX_BYTES = 4_096;

/** Build the fire-and-forget reporter an integration publishes state through.
 *  Every call queues the newest report and drains in the background: delivery
 *  attempts never block the host agent, a failed attempt is retried once at
 *  the longer timeout, and only the LAST queued state matters because the
 *  server keeps no history. */
export function createAgentReporter(
	config: AgentReporterConfig,
): ((state: AgentReportState, message?: string, active?: boolean) => void) {
	let queuedReport: QueuedAgentReport | undefined;
	let sendInFlight = false;

	const sendAttempt = (report: QueuedAgentReport, timeoutMs: number): Promise<boolean> => {
		const request = {
			version: 1,
			capability: config.capability,
			method: "agent.report",
			params: {
				session_id: config.sessionId,
				state: report.state,
				message: report.message,
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
		queuedReport = { state, message, active };
		if (!sendInFlight) void drain();
	};
}

/** Report one official conversation reference exactly once. Explicit server
 * rejection is definite; after request write, every missing/malformed response
 * is ambiguous and MUST NOT be retried by the integration. */
export function reportAgentReference(
	config: AgentReporterConfig,
	reference: AgentReferenceReportValue | null,
	timeoutMs = REFERENCE_REPORT_TIMEOUT_MS,
): Promise<AgentReferenceReportOutcome> {
	const request = {
		version: 1,
		capability: config.capability,
		method: "agent.reference",
		params: {
			session_id: config.sessionId,
			reference,
		},
	};
	const deferred = Promise.withResolvers<AgentReferenceReportOutcome>();
	const socket = net.createConnection(config.endpoint);
	let settled = false;
	let requestWritten = false;
	let responseBuffer = "";
	let timer: ReturnType<typeof setTimeout> | undefined;
	const finish = (outcome: AgentReferenceReportOutcome): void => {
		if (settled) return;
		settled = true;
		clearTimeout(timer);
		socket.destroy();
		deferred.resolve(outcome);
	};
	const missingResponse = (
		reason: "disconnect" | "timeout",
	): AgentReferenceReportOutcome => requestWritten
		? { status: "ambiguous", reason }
		: { status: "rejected", error: "connection_failed" };
	socket.setEncoding("utf8");
	socket.on("connect", () => {
		requestWritten = true;
		socket.write(`${JSON.stringify(request)}\n`);
	});
	socket.on("data", (chunk: string) => {
		responseBuffer += chunk;
		if (Buffer.byteLength(responseBuffer) > REFERENCE_RESPONSE_MAX_BYTES) {
			finish({ status: "ambiguous", reason: "invalid_response" });
			return;
		}
		const newline = responseBuffer.indexOf("\n");
		if (newline < 0) return;
		let decoded: unknown;
		try {
			decoded = JSON.parse(responseBuffer.slice(0, newline));
		} catch {
			finish({ status: "ambiguous", reason: "invalid_response" });
			return;
		}
		if (
			decoded !== null && typeof decoded === "object" &&
			Object.keys(decoded).length === 1 && "ok" in decoded &&
			decoded.ok === true
		) {
			finish({ status: "acknowledged" });
			return;
		}
		if (
			decoded !== null && typeof decoded === "object" &&
			"ok" in decoded && decoded.ok === false &&
			"error" in decoded && typeof decoded.error === "string"
		) {
			finish({ status: "rejected", error: decoded.error });
			return;
		}
		finish({ status: "ambiguous", reason: "invalid_response" });
	});
	socket.on("error", () => finish(missingResponse("disconnect")));
	socket.on("end", () => finish(missingResponse("disconnect")));
	timer = setTimeout(
		() => finish(missingResponse("timeout")),
		timeoutMs,
	);
	timer.unref?.();
	return deferred.promise;
}
