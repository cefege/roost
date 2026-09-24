// Connect router ASSEMBLY — wires the CoordinatorService. Every RPC group
// lives in a sibling connect/handlers-*.ts file (400-line cap); each returns a
// Pick<ServiceImpl<…>> spread into the SINGLE router.service() literal below.
// This file owns only auth interception and spread assembly. No handler logic
// lives here.

import { createConnectRouter } from "@connectrpc/connect";
import type { ConnectRouter } from "@connectrpc/connect";

import { CoordinatorService } from "@roost/protocol/proto/coordinator_pb";
import { makeTranscriptionHandlers } from "./handlers-transcription.ts";
import { makeAgentConfigHandlers } from "../agents/handlers-agent-config.ts";
import { makeAgentStatusHandlers } from "../agents/handlers-agent-status.ts";
import { makeAgentPromptHandlers } from "../agents/handlers-agent-prompt.ts";
import { makeAttachmentHandlers } from "../attachments/handlers-attachments.ts";
import { makeAttachmentDirectHandlers } from "../attachments/handlers-attachments-direct.ts";
import { makeAttachmentPeerHandlers } from "../attachments/handlers-attachments-peer.ts";
import { makeMcpHandlers } from "../sessions/handlers-mcp.ts";
import { makeAuthHandlers } from "../auth/handlers-auth.ts";
import { makeSystemHandlers } from "./handlers-system.ts";
import { makeWorkspaceHandlers } from "../sessions/handlers-workspaces.ts";
import { makeTaskHandlers } from "../sessions/handlers-tasks.ts";
import { makeWorkerHandlers } from "../workers/handlers-workers.ts";
import { makeWorkerUpdateHandlers } from "../deploy/handlers-workers-update.ts";
import { makeSessionHandlers } from "../sessions/handlers-sessions.ts";
import { makeStreamingHandlers } from "./handlers-streaming.ts";
import { makeUiHandlers } from "../ui-state/handlers-ui.ts";
import { makePushHandlers } from "../push/handlers-push.ts";

import type { KyselyDB } from "../db/connection.ts";
import type { Database } from "bun:sqlite";
import type { CoordinatorWriteGate } from "../coordinator-write-gate.ts";
import type { CoordConfig } from "@roost/host/config";
import type { JwtCache } from "../auth/jwt.ts";
import { makeAuthInterceptor } from "../auth/auth-interceptor.ts";
import type { PendingEventPublicationStore } from "../events/pending-event-publications.ts";
import type { UiLayoutApplyOwner } from "../ui-state/ui-layout-apply-owner.ts";
import type { UiStateOwner } from "../ui-state/ui-state-owner.ts";
import type { SelfHostedTenant } from "../auth/self-hosted-tenant.ts";
import type { CloudflareAccessGate } from "../auth/cf-access.ts";
import type { TerminalGrantOwner } from "../terminal/direct/terminal-grant-owner.ts";
import type { AttachmentGrantOwner } from "../attachments/attachment-grant-owner.ts";
import type { TerminalPeerNegotiations } from "../terminal/direct/terminal-peer-negotiations.ts";
import type { AttachmentPeerNegotiations } from "../attachments/attachment-peer-negotiations.ts";
import type { AttachmentDirectStatusResults } from "../attachments/attachment-direct-status-results.ts";
import type { TerminalInputRouteResults } from "../terminal/input/terminal-input-route-results.ts";

// ─── deps + helpers ───────────────────────────────────────────────────────

export interface ConnectDeps {
  db: KyselyDB;
  sqlite: Database;
  cfg: CoordConfig;
  jwtCache: JwtCache;
  uiLayoutApplies: UiLayoutApplyOwner;
  uiStates: UiStateOwner;
  /** Required: the keeper-update fence only holds if every mutation path
   * leases the same gate instance. */
  writeGate: CoordinatorWriteGate;
  /** Required: the single self-hosted account/organization/dashboard resolved
   * once at startup. Every scoped write takes its value from here. */
  selfHostedTenant: SelfHostedTenant;
  /** Optional browser front-door verifier, derived once from cfg. */
  cfAccess: CloudflareAccessGate | null;
  pendingPublications?: PendingEventPublicationStore;
  /** Factory-owned direct-terminal leases; handlers never construct a second registry. */
  terminalGrants: TerminalGrantOwner;
  /** Factory-owned direct attachment grants; they never share terminal state. */
  attachmentGrants: AttachmentGrantOwner;
  /** Factory-owned typed peer signaling; worker frames settle only this owner. */
  terminalPeerNegotiations: TerminalPeerNegotiations;
  /** Factory-owned attachment signaling; worker frames settle only this owner. */
  attachmentPeerNegotiations: AttachmentPeerNegotiations;
  /** Factory-owned typed attachment status correlation. */
  attachmentDirectStatusResults: AttachmentDirectStatusResults;
  /** Factory-owned typed route controls; absent only in isolated legacy fixtures. */
  terminalInputRouteResults?: TerminalInputRouteResults;
  /** Deterministic observation point immediately before the keeper-update
   * handler's final empty-session query. */
  _onKeeperUpdateFinalEmptyRecheck?: () => void;
  onKeyRevoked?: (fingerprint: string) => void;
  /** Synchronous post-commit worker fence: revoke every admitted generation,
   * detach its ordered inbound queue, then unregister the current handle. */
  onWorkerDeletedFence?: (fingerprint: string) => void;
  /** Remove a tombstoned worker from already-open mutable Sync resource
   * indexes. */
  onWorkerDeletedSyncScope?: (fingerprint: string) => void;
  /** Request transport close only after every in-process deletion cleanup. */
  onWorkerDeletedSocketClose?: (fingerprint: string) => void;
}

// ─── ConnectRouter build ──────────────────────────────────────────────────

export function buildConnectRouter(deps: ConnectDeps): ConnectRouter {
  const interceptor = makeAuthInterceptor({
    db: deps.db,
    jwtCache: deps.jwtCache,
    cfg: deps.cfg,
    writeGate: deps.writeGate,
    selfHostedTenant: deps.selfHostedTenant,
  });

  const router = createConnectRouter({
    interceptors: [interceptor],
    // NO acceptCompression. connect-node's brotli/gzip route through Bun's
    // node:zlib, which SEGFAULTS the whole coord process (10 crashes 2026-06-27,
    // dumps show node:zlib loaded + corrupted-pointer address) → workers get 502
    // / ws-error and can't attach. Same class as the worker↔coord raw-WS rule:
    // do not run connect-node's zlib compression under Bun. Static SPA assets
    // are still compressed in main.ts (one-shot buffer, a different path).
  });

  router.service(CoordinatorService, {
    // Unary + server-streaming handlers, split by domain into sibling
    // connect/handlers-*.ts files (400-line cap). Each factory returns a
    // Pick<ServiceImpl<…>> spread into THIS one object literal — a SEPARATE
    // router.service() call per domain shadows the rest with unimplemented-
    // throws (connect stubs every absent method).
    ...makeWorkerHandlers(deps),
    ...makeWorkerUpdateHandlers(deps),
    ...makeSessionHandlers(deps),
    ...makeAgentStatusHandlers(deps),
    ...makeAgentPromptHandlers(deps),
    ...makeWorkspaceHandlers(deps),
    ...makeTaskHandlers(deps),
    ...makeMcpHandlers(deps),
    ...makeAuthHandlers(deps),
    ...makeSystemHandlers(deps),
    ...makeTranscriptionHandlers(deps),
    ...makeAgentConfigHandlers(deps),
    ...makeAttachmentHandlers(deps),
    ...makeAttachmentDirectHandlers(deps),
    ...makeAttachmentPeerHandlers(deps),
    ...makeUiHandlers(deps),
    ...makePushHandlers(deps),
    ...makeStreamingHandlers(deps),
  });

  // coord↔worker transport is the raw WebSocket at /ws/coord-worker/:fp
  // (worker-ws-handler.ts, wired in main.ts). The Connect bidi
  // WorkerService.Attach it replaced is gone — Connect bidi can't hold a
  // stable full-duplex stream under Bun (no h2; h1.1 buffers the upstream),
  // which hung every sessionsSpawn. See worker-ws-handler.ts header.

  return router;
}
