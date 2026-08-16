// Roost-owned integration adapted from Herdr at commit
// eacea2daf0b72973173b728936b27478374f2cd2 (Apache-2.0).
// ROOST_INTEGRATION_ID=pi ROOST_INTEGRATION_VERSION=2
// @ts-nocheck

import net from "node:net";

const endpoint = process.env.ROOST_AGENT_ENDPOINT;
const capability = process.env.ROOST_AGENT_CAPABILITY;
const sessionId = process.env.ROOST_SESSION_ID;
const disabled = process.env.ROOST_AGENT_STATUS_DISABLED === "1"
  || !endpoint || !capability || !sessionId;
const parsedHeartbeatMs = Number.parseInt(process.env.ROOST_AGENT_HEARTBEAT_MS ?? "", 10);
const heartbeatMs = Number.isFinite(parsedHeartbeatMs) && parsedHeartbeatMs >= 0
  ? parsedHeartbeatMs : 10_000;

type AgentState = "working" | "blocked" | "idle";
type QueuedReport = { state: AgentState; message?: string; active: boolean; seq: number };
interface PiContext { mode?: unknown; isIdle?: () => unknown }
interface BlockedEvent { active?: unknown; label?: unknown }
interface PiApi {
  events: { on(name: string, handler: (data: unknown) => void): void };
  on(name: string, handler: (event: unknown, context: unknown) => void): void;
}

let reportSeq = Date.now() * 1_000;
let queuedReport: QueuedReport | undefined;
let sendInFlight = false;


function nextSeq(): number {
  reportSeq++;
  return reportSeq;
}

function sendAttempt(report: QueuedReport, timeoutMs: number): Promise<boolean> {
  if (disabled) return Promise.resolve(true);
  const request = {
    version: 1,
    capability,
    method: "agent.report",
    params: {
      session_id: sessionId,
      pid: process.pid,
      agent: "pi",
      state: report.state,
      message: report.message,
      seq: report.seq,
      active: report.active,
    },
  };
  const { promise, resolve } = Promise.withResolvers<boolean>();
  const socket = net.createConnection(endpoint);
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
}

async function sendNow(report: QueuedReport): Promise<void> {
  if (await sendAttempt(report, 500)) return;
  await sendAttempt(report, 1_500);
}

function queueReport(state: AgentState, message: string | undefined, active = true): void {
  queuedReport = { state, message, active, seq: nextSeq() };
  if (!sendInFlight) void drainReports();
}

async function drainReports(): Promise<void> {
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
    if (queuedReport) void drainReports();
  }
}

export default function install(pi: PiApi): void {
  if (disabled) return;
  let rootSession = false;
  let agentActive = false;
  let blockedCount = 0;
  let blockedMessage: string | undefined;
  let lastState: AgentState | undefined;
  let lastMessage: string | undefined;
  let heartbeatTimer: ReturnType<typeof setInterval> | undefined;

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
