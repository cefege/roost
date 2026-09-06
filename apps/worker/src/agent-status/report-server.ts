// Local socket server installed omp/pi extensions report into. Session
// capabilities authorize claims; kernel peer PID plus a fresh process scan own
// identity. Durable reference ACK follows SQLite append, never mere receipt.
import net from "node:net";
import {
  cleanupLocalEndpoint,
  LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS,
  LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES,
  LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT_MS,
  prepareLocalEndpoint,
  secureLocalEndpoint,
  type LocalEndpoint,
} from "@roost/shared/local-endpoint";
import { log } from "@roost/shared/log";
import { supportedHostPlatform } from "@roost/shared/platform";
import type { AgentScreenDetector } from "./detector.ts";
import type { AgentStatusRegistry } from "./registry.ts";
import {
  createLocalPeerProcessIdReader,
  type LocalPeerProcessIdReader,
} from "./peer-process-id.ts";
import {
  resolveAgentReportEndpoint,
  verifyAgentReportCapability,
} from "./environment.ts";
import type { SessionEventSink } from "../event-sink.ts";
import type { AgentReferenceAdmissionGate } from "./reference-admission.ts";
import {
  AGENT_REPORT_MAX_LINE_BYTES,
  AgentIntegrationRequestSchema,
  type AgentReferenceReportRequest,
  type AgentStatusReportRequest,
} from "./report-protocol.ts";

const MAX_REQUESTS_PER_CONNECTION = 1;


export interface AgentReportServerOptions {
  detector: Pick<AgentScreenDetector, "reportingAgentForSession">;
  registry: Pick<AgentStatusRegistry, "reportIntegration">;
  eventSink: Pick<
    SessionEventSink,
    "reserveSessionEvent" | "releaseSessionEvent" | "emit"
  >;
  referenceAdmission: Pick<AgentReferenceAdmissionGate, "runExclusive">;
  peerProcessIdReader?: Pick<LocalPeerProcessIdReader, "read">;
  endpoint?: LocalEndpoint;
  /** POSIX-only explicit address seam for isolated callers. */
  socketPath?: string;
}

export interface AgentReportServer {
  endpoint: LocalEndpoint;
  path: string;
  close(): Promise<void>;
}

function response(ok: boolean, error?: string): string {
  return `${JSON.stringify(ok ? { ok: true } : { ok: false, error })}\n`;
}

export async function startAgentReportServer(
  options: AgentReportServerOptions,
): Promise<AgentReportServer> {
  let endpoint = options.endpoint ?? resolveAgentReportEndpoint();
  if (options.socketPath) {
    if (supportedHostPlatform() === "win32") {
      throw new Error("socketPath cannot select a UDS on Windows");
    }
    endpoint = {
      ...endpoint,
      kind: "uds",
      address: options.socketPath,
      isFilesystemPath: true,
    };
  }
  await prepareLocalEndpoint(endpoint);
  const ownedPeerProcessIdReader = options.peerProcessIdReader
    ? null
    : createLocalPeerProcessIdReader({ platform: endpoint.platform });
  const peerProcessIdReader = options.peerProcessIdReader ?? ownedPeerProcessIdReader!;
  if (ownedPeerProcessIdReader && !ownedPeerProcessIdReader.available) {
    log.warn("agent-status", "peer_process_attestation_unavailable", {
      platform: endpoint.platform,
    });
  }
  let integrationSeq = Math.floor(Date.now() * 1_000);
  let admissionTail: Promise<void> = Promise.resolve();
  const admitReport = (
    request: AgentStatusReportRequest,
    reporterPid: number,
  ): Promise<string | undefined> => {
    const admitted = admissionTail.then(async () => {
      const sessionId = request.params.session_id;
      const identity = await options.detector.reportingAgentForSession(
        sessionId,
        reporterPid,
      );
      if (!identity) return "reporter_identity_mismatch";
      const nextSeq = Math.max(integrationSeq + 1, Math.floor(Date.now() * 1_000));
      if (!Number.isSafeInteger(nextSeq)) throw new Error("agent report sequence exhausted");
      integrationSeq = nextSeq;
      const accepted = options.registry.reportIntegration({
        sessionId,
        agentId: identity.agentId,
        state: request.params.state,
        processId: identity.pid,
        message: request.params.message,
        seq: nextSeq,
        active: request.params.active,
      });
      return accepted ? undefined : "stale_report";
    });
    admissionTail = admitted.then(() => undefined, () => undefined);
    return admitted;
  };
  const admitReference = (
    request: AgentReferenceReportRequest,
    reporterPid: number,
  ): Promise<string | undefined> => options.referenceAdmission.runExclusive(
    async () => {
      const sessionId = request.params.session_id;
      const identity = await options.detector.reportingAgentForSession(
        sessionId,
        reporterPid,
      );
      if (!identity) return "reporter_identity_mismatch";
      if (identity.agentId !== "omp") return "unsupported_agent";
      const reservation = options.eventSink.reserveSessionEvent(
        "agent_reference",
      );
      try {
        options.eventSink.emit({
          kind: "agent_reference",
          session_id: sessionId,
          reference: request.params.reference,
          ts: Date.now(),
        }, reservation);
      } catch (error) {
        try {
          options.eventSink.releaseSessionEvent(reservation);
        } catch {
          // Append may have consumed the reservation before transport failed.
        }
        throw error;
      }
      log.info("agent-reference", "reference_report_committed", {
        session_id: sessionId,
        agent_id: identity.agentId,
        action: request.params.reference === null ? "clear" : "set",
      });
      return undefined;
    },
  );
  let unauthenticatedConnections = 0;
  const server = net.createServer((socket) => {
    let reporterPid: number | null = null;
    try {
      reporterPid = peerProcessIdReader.read(socket);
    } catch {
      // An injected or platform-native reader failure is an authentication failure.
    }
    if (reporterPid === null) {
      log.warn("agent-status", "peer_process_attestation_failed", {
        platform: endpoint.platform,
      });
      socket.destroy();
      return;
    }
    if (
      unauthenticatedConnections >=
      LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS
    ) {
      socket.destroy();
      return;
    }
    unauthenticatedConnections++;
    let awaitingAuthentication = true;
    let authenticatedSessionId: string | null = null;
    let unauthenticatedBytes = 0;
    const releaseUnauthenticatedSlot = () => {
      if (!awaitingAuthentication) return;
      awaitingAuthentication = false;
      unauthenticatedConnections = Math.max(0, unauthenticatedConnections - 1);
    };
    const authenticationTimer = setTimeout(() => {
      if (awaitingAuthentication) socket.destroy();
    }, LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT_MS);
    authenticationTimer.unref?.();
    socket.once("close", () => {
      clearTimeout(authenticationTimer);
      releaseUnauthenticatedSlot();
    });
    socket.setEncoding("utf8");
    let buffer = "";
    let requestCount = 0;
    let chain = Promise.resolve();
    const send = (body: string) => {
      if (!socket.destroyed && socket.writable) socket.write(body);
    };
    const handleLine = async (line: string) => {
      if (Buffer.byteLength(line) > AGENT_REPORT_MAX_LINE_BYTES) {
        send(response(false, "request_too_large"));
        socket.destroy();
        return;
      }
      let raw: unknown;
      try { raw = JSON.parse(line); }
      catch {
        socket.end(response(false, "invalid_json"));
        return;
      }
      const parsed = AgentIntegrationRequestSchema.safeParse(raw);
      if (!parsed.success) {
        socket.end(response(false, "invalid_request"));
        return;
      }
      const claimed = parsed.data.params.session_id;
      if (authenticatedSessionId === null) {
        if (!verifyAgentReportCapability(endpoint, claimed, parsed.data.capability)) {
          send(response(false, "authentication_failed"));
          socket.destroy();
          return;
        }
        authenticatedSessionId = claimed;
        clearTimeout(authenticationTimer);
        releaseUnauthenticatedSlot();
      } else if (
        claimed !== authenticatedSessionId ||
        !verifyAgentReportCapability(endpoint, claimed, parsed.data.capability)
      ) {
        send(response(false, "authentication_failed"));
        socket.destroy();
        return;
      }
      try {
        const admissionError = parsed.data.method === "agent.report"
          ? await admitReport(parsed.data, reporterPid)
          : await admitReference(parsed.data, reporterPid);
        socket.end(admissionError ? response(false, admissionError) : response(true));
      } catch {
        if (parsed.data.method === "agent.reference") {
          log.warn("agent-reference", "reference_report_failed", {
            session_id: claimed,
            outcome: "ambiguous",
          });
          socket.destroy();
        } else {
          log.warn("agent-status", "report_request_failed", {
            outcome: "internal_error",
          });
          socket.end(response(false, "internal_error"));
        }
      }
    };
    socket.on("data", (chunk: string) => {
      try {
        if (awaitingAuthentication) {
          unauthenticatedBytes += Buffer.byteLength(chunk);
          if (unauthenticatedBytes > LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES) {
            send(response(false, "request_too_large"));
            socket.destroy();
            return;
          }
        }
        buffer += chunk;
        if (Buffer.byteLength(buffer) > AGENT_REPORT_MAX_LINE_BYTES * 2) {
          send(response(false, "request_too_large"));
          socket.destroy();
          return;
        }
        let newline = buffer.indexOf("\n");
        while (newline >= 0) {
          const line = buffer.slice(0, newline);
          buffer = buffer.slice(newline + 1);
          requestCount++;
          if (requestCount > MAX_REQUESTS_PER_CONNECTION) {
            send(response(false, "too_many_requests"));
            socket.destroy();
            return;
          }
          if (line.trim()) chain = chain.then(() => handleLine(line)).catch(() => undefined);
          newline = buffer.indexOf("\n");
        }
      } catch {
        send(response(false, "invalid_request"));
        socket.destroy();
      }
    });
    socket.on("error", () => undefined);
  });
  const { promise, resolve, reject } = Promise.withResolvers<void>();
  server.once("error", reject);
  server.listen(endpoint.address, resolve);
  try {
    await promise;
    server.removeListener("error", reject);
    server.on("error", (error) => {
      log.warn("agent-status", "report_server_error", { error: String(error) });
    });
    await secureLocalEndpoint(endpoint);
  } catch (error) {
    ownedPeerProcessIdReader?.close();
    try { server.close(); } catch { /* listen failed before binding */ }
    await cleanupLocalEndpoint(endpoint);
    throw error;
  }
  return {
    endpoint,
    path: endpoint.address,
    async close(): Promise<void> {
      const closed = Promise.withResolvers<void>();
      server.close(() => closed.resolve());
      await closed.promise;
      ownedPeerProcessIdReader?.close();
      await cleanupLocalEndpoint(endpoint);
    },
  };
}
