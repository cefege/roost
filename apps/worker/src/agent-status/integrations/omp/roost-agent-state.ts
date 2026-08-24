// Roost-owned integration adapted from Herdr at commit
// eacea2daf0b72973173b728936b27478374f2cd2 (Apache-2.0).
// ROOST_INTEGRATION_ID=omp ROOST_INTEGRATION_VERSION=2
//
// Owns ONLY the omp event→state mapping (debounce, retry grace, nested
// blockers). Delivery is the shared report transport; this source ships as a
// standalone extension file, so standalone-integration.ts splices
// report-transport.ts in place of the import below at embed/install time.

import { createAgentReporter, type AgentReportState as AgentState } from "../../report-transport.ts";

interface PiApi {
  events: { on(name: string, handler: (data: unknown) => void): void };
  on(name: string, handler: (event: unknown, context: unknown) => void): void;
}
interface AgentContext { hasUI?: boolean; isIdle?: () => unknown }
interface AssistantMessage { role?: unknown; stopReason?: unknown; errorMessage?: unknown }
interface AgentEndEvent { messages?: unknown }
interface AskArgs { questions?: Array<{ question?: unknown }> }
interface ToolEvent { toolName?: unknown; reason?: unknown; args?: unknown }
interface BlockedEvent { active?: unknown; label?: unknown }

const retryableErrorPattern = /overloaded|provider.?returned.?error|rate.?limit|too many requests|429|500|502|503|504|service.?unavailable|server.?error|internal.?error|network.?error|connection.?error|connection.?refused|connection.?lost|websocket.?closed|websocket.?error|other side closed|fetch failed|upstream.?connect|reset before headers|socket hang up|ended without|http2 request did not get a response|timed? out|timeout|terminated|retry delay/i;

function durationEnv(name: string, fallback: number): number {
  const parsed = Number.parseInt(process.env[name] ?? "", 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

function createReporter(): (state: AgentState, message?: string, active?: boolean) => void {
  const endpoint = process.env.ROOST_AGENT_ENDPOINT;
  const capability = process.env.ROOST_AGENT_CAPABILITY;
  const sessionId = process.env.ROOST_SESSION_ID;
  // An incomplete endpoint triple means agent status was never provisioned on
  // this machine; degrade to a no-op rather than crash the host agent.
  if (!endpoint || !capability || !sessionId) return () => {};
  return createAgentReporter({ agent: "omp", endpoint, capability, sessionId });
}

function lastAssistantMessage(messages: unknown[]): AssistantMessage | undefined {
  for (let index = messages.length - 1; index >= 0; index--) {
    const message = messages[index] as AssistantMessage;
    if (message.role === "assistant") return message;
  }
  return undefined;
}

function retryableErrorMessage(value: unknown): string | undefined {
  const event = value as AgentEndEvent;
  const assistant = lastAssistantMessage(Array.isArray(event.messages) ? event.messages : []);
  if (assistant?.stopReason !== "error") return undefined;
  const message = String(assistant.errorMessage ?? "");
  return retryableErrorPattern.test(message) ? (message || "retryable provider error") : undefined;
}

function askBlockedMessage(value: unknown): string {
  const args = value as AskArgs;
  const questions = Array.isArray(args.questions) ? args.questions : [];
  const first = questions.find((question) => typeof question.question === "string");
  return typeof first?.question === "string" ? first.question : "waiting for user input";
}

export default function install(pi: PiApi): void {
  if (process.env.ROOST_AGENT_STATUS_DISABLED === "1") return;
  const queueReport = createReporter();
  const idleDebounceMs = durationEnv("ROOST_OMP_IDLE_DEBOUNCE_MS", 250);
  const retryGraceMs = durationEnv("ROOST_OMP_RETRY_GRACE_MS", 2_500);
  const heartbeatMs = durationEnv("ROOST_AGENT_HEARTBEAT_MS", 10_000);
  let rootSession = false;
  let agentActive = false;
  let retryHoldActive = false;
  let failureBlocked = false;
  let failureMessage: string | undefined;
  let blockedCount = 0;
  let blockedMessage: string | undefined;
  let lastState: AgentState | undefined;
  let lastMessage: string | undefined;
  let idleTimer: ReturnType<typeof setTimeout> | undefined;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let heartbeatTimer: ReturnType<typeof setInterval> | undefined;

  const clearTimers = () => {
    clearTimeout(idleTimer);
    clearTimeout(retryTimer);
    idleTimer = undefined;
    retryTimer = undefined;
  };
  const clearFailure = () => {
    retryHoldActive = false;
    failureBlocked = false;
    failureMessage = undefined;
  };
  const desiredState = (): { state: AgentState; message?: string } => {
    if (blockedCount > 0) return { state: "blocked", message: blockedMessage };
    if (failureBlocked) return { state: "blocked", message: failureMessage };
    if (agentActive || retryHoldActive) return { state: "working" };
    return { state: "idle" };
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
  const activateRoot = (value: unknown): boolean => {
    const context = value as AgentContext;
    if (context.hasUI !== true) return false;
    rootSession = true;
    startHeartbeat();
    return true;
  };
  const activateBlocked = (message?: string) => {
    clearTimers();
    blockedCount++;
    blockedMessage = message;
    publishState();
  };
  const deactivateBlocked = () => {
    blockedCount = Math.max(0, blockedCount - 1);
    if (blockedCount === 0) blockedMessage = undefined;
    publishState();
  };
  const resetSession = () => {
    clearTimers();
    clearFailure();
    agentActive = false;
    blockedCount = 0;
    blockedMessage = undefined;
  };

  pi.events.on("herdr:blocked", (value) => {
    if (!rootSession) return;
    const data = value as BlockedEvent;
    if (data.active) activateBlocked(typeof data.label === "string" ? data.label : undefined);
    else deactivateBlocked();
  });
  pi.on("session_start", (_event, value) => {
    if (!activateRoot(value)) return;
    const context = value as AgentContext;
    agentActive = context.isIdle?.() === false;
    publishState(true);
  });
  pi.on("session_switch", (_event, context) => {
    if (!activateRoot(context)) return;
    resetSession();
    publishState(true);
  });
  pi.on("agent_start", (_event, context) => {
    if (!rootSession && !activateRoot(context)) return;
    clearTimers();
    clearFailure();
    agentActive = true;
    publishState();
  });
  pi.on("tool_approval_requested", (value, context) => {
    if (!rootSession && !activateRoot(context)) return;
    const event = value as ToolEvent;
    const reason = typeof event.reason === "string" ? event.reason : undefined;
    const toolName = typeof event.toolName === "string" ? event.toolName : "Tool";
    activateBlocked(reason || `${toolName} approval`);
  });
  pi.on("tool_approval_resolved", (_event, context) => {
    if (!rootSession && !activateRoot(context)) return;
    deactivateBlocked();
  });
  pi.on("tool_execution_start", (value, context) => {
    const event = value as ToolEvent;
    if (event.toolName !== "ask" || (!rootSession && !activateRoot(context))) return;
    activateBlocked(askBlockedMessage(event.args));
  });
  pi.on("tool_execution_end", (value, context) => {
    const event = value as ToolEvent;
    if (event.toolName !== "ask" || (!rootSession && !activateRoot(context))) return;
    deactivateBlocked();
  });
  pi.on("agent_end", (event) => {
    if (!rootSession || !agentActive) return;
    agentActive = false;
    const retryMessage = retryableErrorMessage(event);
    if (retryMessage) {
      clearTimers();
      retryHoldActive = true;
      failureMessage = retryMessage;
      publishState();
      retryTimer = setTimeout(() => {
        retryTimer = undefined;
        retryHoldActive = false;
        failureBlocked = true;
        publishState();
      }, retryGraceMs);
      retryTimer.unref?.();
      return;
    }
    clearTimers();
    clearFailure();
    idleTimer = setTimeout(() => {
      idleTimer = undefined;
      publishState();
    }, idleDebounceMs);
    idleTimer.unref?.();
  });
  pi.on("session_shutdown", () => {
    if (!rootSession) return;
    clearTimers();
    clearInterval(heartbeatTimer);
    heartbeatTimer = undefined;
    rootSession = false;
    queueReport(lastState ?? "idle", lastMessage, false);
  });
}
