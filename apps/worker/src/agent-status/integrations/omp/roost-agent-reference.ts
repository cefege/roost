// Roost-owned OMP v18.1.12 conversation-reference integration.
// ROOST_INTEGRATION_ID=omp-reference ROOST_INTEGRATION_VERSION=1
// Uses only official session hooks/getters, prefers the opaque session path,
// and awaits one durable local report attempt without reading transcript data.

import {
  reportAgentReference,
  type AgentReferenceReportValue,
  type AgentReporterConfig,
} from "../../report-transport.ts";

interface OmpSessionManager {
  getSessionFile(): string | undefined;
  getSessionId(): string;
}

interface OmpExtensionContext {
  sessionManager?: OmpSessionManager;
}

interface OmpSessionStopEvent {
  session_id?: unknown;
  session_file?: unknown;
}

interface OmpExtensionApi {
  on(
    name: "session_start" | "session_switch" | "session_stop",
    handler: (event: unknown, context: OmpExtensionContext) => unknown,
  ): void;
}

function reporterConfig(): AgentReporterConfig | null {
  const endpoint = process.env.ROOST_AGENT_ENDPOINT;
  const capability = process.env.ROOST_AGENT_CAPABILITY;
  const sessionId = process.env.ROOST_SESSION_ID;
  if (!endpoint || !capability || !sessionId) return null;
  return { endpoint, capability, sessionId };
}

function referenceFromContext(
  context: OmpExtensionContext,
): AgentReferenceReportValue | null {
  const sessionManager = context.sessionManager;
  if (!sessionManager) return null;
  const sessionFile = sessionManager.getSessionFile();
  if (typeof sessionFile === "string" && sessionFile.length > 0) {
    return { kind: "path", value: sessionFile };
  }
  const sessionId = sessionManager.getSessionId();
  return typeof sessionId === "string" && sessionId.length > 0
    ? { kind: "id", value: sessionId }
    : null;
}

function referenceFromStopEvent(
  event: unknown,
): AgentReferenceReportValue | null {
  if (!event || typeof event !== "object") return null;
  const stopped = event as OmpSessionStopEvent;
  if (
    typeof stopped.session_file === "string" &&
    stopped.session_file.length > 0
  ) {
    return { kind: "path", value: stopped.session_file };
  }
  return typeof stopped.session_id === "string" && stopped.session_id.length > 0
    ? { kind: "id", value: stopped.session_id }
    : null;
}

export default function install(omp: OmpExtensionApi): void {
  const config = reporterConfig();
  if (!config) return;
  const reportCurrent = async (
    _event: unknown,
    context: OmpExtensionContext,
  ): Promise<void> => {
    const reference = referenceFromContext(context);
    if (reference) await reportAgentReference(config, reference);
  };
  omp.on("session_start", reportCurrent);
  omp.on("session_switch", reportCurrent);
  omp.on("session_stop", async (event, context) => {
    const reference = referenceFromStopEvent(event) ??
      referenceFromContext(context);
    if (reference) await reportAgentReference(config, reference);
  });
}
