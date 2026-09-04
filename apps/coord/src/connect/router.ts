// Connect router ASSEMBLY — wires the CoordinatorService. Every RPC group
// lives in a sibling connect/handlers-*.ts file (400-line cap); each returns a
// Pick<ServiceImpl<…>> spread into the SINGLE router.service() literal below.
// This file owns only auth interception and spread assembly. No handler logic
// lives here.

import { createConnectRouter } from "@connectrpc/connect";
import type { ConnectRouter } from "@connectrpc/connect";

import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import { makeTranscriptionHandlers } from "./handlers-transcription.ts";
import { makeAgentConfigHandlers } from "./handlers-agent-config.ts";
import { makeAttachmentHandlers } from "./handlers-attachments.ts";
import { makeMcpHandlers } from "./handlers-mcp.ts";
import { makeAuthHandlers } from "./handlers-auth.ts";
import { makeAccountHandlers } from "./handlers-account.ts";
import { makeNativeAuthHandlers } from "./handlers-native-auth.ts";
import { makeFederatedAuthHandlers } from "./handlers-federated-auth.ts";
import { makeRelocationHandlers } from "./handlers-relocation.ts";
import { makeSystemHandlers } from "./handlers-system.ts";
import { makeWorkspaceHandlers } from "./handlers-workspaces.ts";
import { makeTaskHandlers } from "./handlers-tasks.ts";
import { makeWorkerHandlers } from "./handlers-workers.ts";
import { makeSessionHandlers } from "./handlers-sessions.ts";
import { makeStreamingHandlers } from "./handlers-streaming.ts";
import { makeUiHandlers } from "./handlers-ui.ts";
import { makeCoordinatorMoveHandlers } from "./handlers-coordinator-move.ts";
import { makePushHandlers } from "./handlers-push.ts";

import type { KyselyDB } from "../db/connection.ts";
import type { Database } from "bun:sqlite";
import type { CoordKey } from "../coord-key.ts";
import type { CoordConfig } from "@roost/shared/config";
import type { JwtCache } from "../jwt.ts";
import { makeAuthInterceptor } from "./auth-interceptor.ts";
import type { CoordinatorMoveService } from "../coord-move/orchestrator.ts";
import type { EmailDeliveryService } from "../email-delivery.ts";
import type { PasswordWorkGate } from "./password-work-gate.ts";
import type { PendingEventPublicationStore } from "../pending-event-publications.ts";

// ─── deps + helpers ───────────────────────────────────────────────────────

export interface ConnectDeps {
  db: KyselyDB;
  sqlite: Database;
  coordKey: CoordKey;
  cfg: CoordConfig;
  jwtCache: JwtCache;
  passwordWorkGate: PasswordWorkGate;
  move?: CoordinatorMoveService;
  pendingPublications?: PendingEventPublicationStore;
  onKeyRevoked?: (fingerprint: string) => void;
  /** Synchronous post-commit worker fence: revoke every admitted generation,
   * detach its ordered inbound queue, then unregister the current handle. */
  onWorkerDeletedFence?: (fingerprint: string) => void;
  /** Remove a tombstoned worker from already-open mutable Sync scopes. */
  onWorkerDeletedSyncScope?: (dashboardId: string, fingerprint: string) => void;
  /** Request transport close only after every in-process deletion cleanup. */
  onWorkerDeletedSocketClose?: (fingerprint: string) => void;
  /** Encrypts password-reset payloads before transactional outbox insert. */
  email?: Pick<EmailDeliveryService, "encryptPayload">;
  /** Invoked after a dashboard membership/status mutation commits. A device
   * fingerprint narrows revocation to that device when present. */
  onDashboardRevoked?: (dashboardId: string, fingerprint?: string) => void;
}

// ─── ConnectRouter build ──────────────────────────────────────────────────

export function buildConnectRouter(deps: ConnectDeps): ConnectRouter {
  const interceptor = makeAuthInterceptor({
    db: deps.db, jwtCache: deps.jwtCache, cfg: deps.cfg, move: deps.move,
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
    ...makeSessionHandlers(deps),
    ...makeWorkspaceHandlers(deps),
    ...makeTaskHandlers(deps),
    ...makeMcpHandlers(deps),
    ...makeAuthHandlers(deps),
    ...makeAccountHandlers(deps),
    ...makeNativeAuthHandlers(deps),
    ...makeFederatedAuthHandlers(deps),
    ...makeRelocationHandlers(deps),
    ...makeSystemHandlers(deps),
    ...makeTranscriptionHandlers(deps),
    ...makeAgentConfigHandlers(deps),
    ...makeAttachmentHandlers(deps),
    ...makeUiHandlers(deps),
    ...makePushHandlers(deps),
    ...makeStreamingHandlers(deps),
    ...makeCoordinatorMoveHandlers(deps),
  });

  // coord↔worker transport is the raw WebSocket at /ws/coord-worker/:fp
  // (worker-ws-handler.ts, wired in main.ts). The Connect bidi
  // WorkerService.Attach it replaced is gone — Connect bidi can't hold a
  // stable full-duplex stream under Bun (no h2; h1.1 buffers the upstream),
  // which hung every sessionsSpawn. See worker-ws-handler.ts header.

  return router;
}
