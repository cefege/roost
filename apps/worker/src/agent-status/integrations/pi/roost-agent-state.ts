// Roost-owned integration adapted from Herdr at commit
// eacea2daf0b72973173b728936b27478374f2cd2 (Apache-2.0).
// ROOST_INTEGRATION_ID=pi ROOST_INTEGRATION_VERSION=2
//
// Owns ONLY the pi event→state mapping (root-session gate, blocked counter,
// heartbeat). Delivery is the shared report transport; this source ships as a
// standalone extension file, so standalone-integration.ts splices
// report-transport.ts in place of the import below at embed/install time.

import { createAgentReporter, type AgentReportState as AgentState } from "../../report-transport.ts";

interface PiContext { mode?: unknown; isIdle?: () => unknown }
interface BlockedEvent { active?: unknown; label?: unknown }
interface PiApi {
  events: { on(name: string, handler: (data: unknown) => void): void };
  on(name: string, handler: (event: unknown, context: unknown) => void): void;
}

export default function install(pi: PiApi): void {
  const endpoint = process.env.ROOST_AGENT_ENDPOINT;
  const capability = process.env.ROOST_AGENT_CAPABILITY;
  const sessionId = process.env.ROOST_SESSION_ID;
  if (process.env.ROOST_AGENT_STATUS_DISABLED === "1" || !endpoint || !capability || !sessionId) return;
  const parsedHeartbeatMs = Number.parseInt(process.env.ROOST_AGENT_HEARTBEAT_MS ?? "", 10);
  const heartbeatMs = Number.isFinite(parsedHeartbeatMs) && parsedHeartbeatMs >= 0
    ? parsedHeartbeatMs : 10_000;

  let rootSession = false;
  let agentActive = false;
  let blockedCount = 0;
  let blockedMessage: string | undefined;
  let lastState: AgentState | undefined;
  let lastMessage: string | undefined;
  let heartbeatTimer: ReturnType<typeof setInterval> | undefined;

  const queueReport = createAgentReporter({ agent: "pi", endpoint, capability, sessionId });
  const desiredState = (): { state: AgentState; message?: string } => {
    if (blockedCount > 0) return { state: "blocked", message: blockedMessage };
    return { state: agentActive ? "working" : "idle" };
  };
  const publishState = (force = false) => {
    const next = desiredState();
    if (!force && next.state === lastState && next.message === lastMessage) return;
    lastState = next.state;
    lastMessage = next.message;
    queueReport(next.state, next.message);
  };
  const startHeartbeat = () => {
    if (heartbeatTimer) return;
    heartbeatTimer = setInterval(() => {
      if (rootSession && lastState) queueReport(lastState, lastMessage);
    }, heartbeatMs);
    heartbeatTimer.unref?.();
  };

  pi.events.on("herdr:blocked", (value) => {
    if (!rootSession) return;
    const data = value as BlockedEvent;
    if (data.active) {
      blockedCount++;
      blockedMessage = typeof data.label === "string" ? data.label : undefined;
    } else {
      blockedCount = Math.max(0, blockedCount - 1);
      if (blockedCount === 0) blockedMessage = undefined;
    }
    publishState();
  });
  pi.on("session_start", (_event, value) => {
    const context = value as PiContext;
    if (context.mode !== "tui") return;
    rootSession = true;
    const isIdle = context.isIdle?.() ?? true;
    agentActive = isIdle === false;
    startHeartbeat();
    publishState(true);
  });
  pi.on("agent_start", () => {
    if (!rootSession) return;
    agentActive = true;
    publishState();
  });
  pi.on("agent_settled", (_event, value) => {
    if (!rootSession) return;
    const context = value as PiContext;
    if (context.isIdle?.() !== true) return;
    agentActive = false;
    publishState();
  });
  pi.on("session_shutdown", () => {
    if (!rootSession) return;
    clearInterval(heartbeatTimer);
    heartbeatTimer = undefined;
    rootSession = false;
    queueReport(lastState ?? "idle", lastMessage, false);
  });
}
