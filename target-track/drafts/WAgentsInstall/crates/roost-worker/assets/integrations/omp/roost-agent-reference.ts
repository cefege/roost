// Roost-owned OMP v18.1.12 conversation-reference integration.
// ROOST_INTEGRATION_ID=omp-reference ROOST_INTEGRATION_VERSION=1
// Official hooks/getters only: session_start, session_switch, and session_stop —
// OMP's turn-settle event (SessionStopEvent in @oh-my-pi/pi-coding-agent
// dist/types/extensibility/shared-events.d.ts), which re-reports the live
// reference each turn. Prefers an absolute session path; reads no transcript.

import path from "node:path";
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

/** The two SessionStopEvent fields this report needs. Both are read
 *  defensively: the event crosses an extension-host boundary. */
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

/** A session file identifies a conversation only when it is absolute, so a
 *  relative one must never shadow the session id — which resumes the very same
 *  conversation and is offered in the same call. */
function referenceFromSessionFile(
  sessionFile: unknown,
): AgentReferenceReportValue | null {
  return typeof sessionFile === "string"
      && (path.posix.isAbsolute(sessionFile) || path.win32.isAbsolute(sessionFile))
    ? { kind: "path", value: sessionFile }
    : null;
}

function referenceFromContext(
  context: OmpExtensionContext,
): AgentReferenceReportValue | null {
  const sessionManager = context.sessionManager;
  if (!sessionManager) return null;
  const fromSessionFile = referenceFromSessionFile(sessionManager.getSessionFile());
  if (fromSessionFile) return fromSessionFile;
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
  const fromSessionFile = referenceFromSessionFile(stopped.session_file);
  if (fromSessionFile) return fromSessionFile;
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
