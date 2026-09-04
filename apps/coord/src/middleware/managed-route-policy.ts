// Managed/public route membership lives here so the listener can focus on auth and dispatch.
// The public surface asks this module to classify requests before any coordinator handler runs.
// It depends on the generated RPC registry and shared sync path rather than duplicating wire facts.
// Unregistered, unlisted, private, or wrong-method requests must remain denied by default.
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import { SYNC_WS_PATH } from "@roost/shared/wire/sync-ws";

export const COORDINATOR_RPC_PREFIX = "/roost.v1.CoordinatorService/";

const REGISTERED_RPC_METHODS = new Set(
  CoordinatorService.methods.map((method) => method.name),
);

export const MANAGED_CREDENTIAL_LIMITS: Record<string, {
  group: string;
  tokensPerWindow: number;
}> = {
  AuthPasswordLogin: {
    group: "public-password-login",
    tokensPerWindow: 10,
  },
  AuthOwnerActivate: {
    group: "public-owner-activation",
    tokensPerWindow: 10,
  },
  AuthPasswordResetRedeem: {
    group: "public-password-reset-redeem",
    tokensPerWindow: 10,
  },
  AuthFederatedContinue: {
    group: "public-federated-continue",
    tokensPerWindow: 10,
  },
  AuthPasswordAdd: {
    group: "protected-password-add",
    tokensPerWindow: 10,
  },
  AuthFederatedLinkBegin: {
    group: "protected-federated-link-begin",
    tokensPerWindow: 10,
  },
  AuthFederatedLink: {
    group: "protected-federated-link",
    tokensPerWindow: 10,
  },
};

// Preserve the existing CF Access-fronted surface when complete Access
// configuration is supplied. Access verifies before this denylist.
const ACCESS_DENIED_PREFIXES = [
  "/internal/",
  "/ws/coord-worker/",
  "/api/db-export",
];

const ACCESS_DENIED_RPCS: Record<string, true | undefined> = {
  "/roost.v1.CoordinatorService/AuthRedeemWorker": true,
  "/roost.v1.CoordinatorService/AuthMintCoordinatorRelocation": true,
  "/roost.v1.CoordinatorService/AuthRedeemCoordinatorRelocation": true,
  "/roost.v1.CoordinatorService/CoordinatorMovePreflight": true,
  "/roost.v1.CoordinatorService/CoordinatorMoveStart": true,
  "/roost.v1.CoordinatorService/CoordinatorMoveStatus": true,
  "/roost.v1.CoordinatorService/MiscDbExportUrl": true,
};

export function isPublicPathDenied(path: string): boolean {
  return ACCESS_DENIED_PREFIXES.some((prefix) => path.startsWith(prefix))
    || ACCESS_DENIED_RPCS[path] === true;
}

/** The only RPCs whose application credential may be supplied by an
 * anonymous managed-mode internet client. Access identity is deliberately
 * absent: it is an optional edge gate, never Roost login identity. */
const MANAGED_PUBLIC_AUTH_RPC_METHODS: Record<string, true | undefined> = {
  AuthCoordIdentity: true,
  AuthPasswordLogin: true,
  AuthOwnerActivate: true,
  AuthPasswordResetRequest: true,
  AuthPasswordResetRedeem: true,
  AuthFederatedContinue: true,
};

/** Worker lifecycle RPCs are reachable through the shared managed edge, but
 * their handlers admit only a verified persisted worker principal. */
const MANAGED_WORKER_RPC_METHODS: Record<string, true | undefined> = {
  WorkersRegister: true,
  WorkersHeartbeat: true,
};

/** Browser/device RPCs admitted to the central typed-principal boundary.
 * Keeping this list exact makes a newly-added RPC private by default. */
const MANAGED_PROTECTED_RPC_METHODS: Record<string, true | undefined> = {
  WorkersList: true,
  WorkersRename: true,
  WorkersDelete: true,
  SessionsList: true,
  SessionsSpawn: true,
  SessionsAttach: true,
  SessionsKill: true,
  SessionsRename: true,
  SessionsInput: true,
  SessionsCursorPos: true,
  SessionsAssignWorkspace: true,
  SessionsGetScrollbackCells: true,
  SessionsSearchScrollback: true,
  WorkspacesList: true,
  WorkspacesCreate: true,
  WorkspacesUpdate: true,
  WorkspacesDelete: true,
  WorkspacesSetSessions: true,
  TasksList: true,
  TasksEnqueue: true,
  TasksNextPending: true,
  TasksSetState: true,
  TasksCancel: true,
  McpList: true,
  McpCreate: true,
  McpDelete: true,
  AuthDashboardAccess: true,
  AuthMintBootstrap: true,
  AuthLogout: true,
  AuthCredentialsGet: true,
  AuthPasswordAdd: true,
  AuthFederatedLinkBegin: true,
  AuthFederatedLink: true,
  DevicesList: true,
  DevicesRevoke: true,
  FilesRead: true,
  FilesReadChunk: true,
  FilesListDir: true,
  FilesMkdir: true,
  TranscriptionGetConfig: true,
  TranscriptionSetConfig: true,
  TranscriptionGrantToken: true,
  TranscriptionTest: true,
  AgentConfigGet: true,
  AgentConfigSet: true,
  UiReportState: true,
  UiListStates: true,
  UiDispatch: true,
  PushGetConfig: true,
  PushSubscribe: true,
  PushUnsubscribe: true,
  AttachFileChunk: true,
  AttachmentProbe: true,
  ListAttachments: true,
  DeleteAttachment: true,
  MiscHealth: true,
};

/** Especially sensitive managed-mode RPCs. Unlisted RPCs are also denied; this
 * table records the private boundaries that must never drift into an allowlist. */
const MANAGED_PRIVATE_RPC_METHODS: Record<string, true | undefined> = {
  AuthRedeemBrowser: true,
  AuthMintCoordinatorRelocation: true,
  AuthRedeemCoordinatorRelocation: true,
  WorkersDeployStart: true,
  WorkersDeployOutput: true,
  DevicesRotateCurrent: true,
  CoordinatorMovePreflight: true,
  CoordinatorMoveStart: true,
  CoordinatorMoveStatus: true,
  PairCreate: true,
  PairPoll: true,
  PairList: true,
  PairApprove: true,
  PairDeny: true,
  MiscDbExportUrl: true,
  MiscMetrics: true,
  DiagDebugLogBatch: true,
  DiagSnapshot: true,
  Sync: true,
};

export type ManagedPublicRouteKind =
  | "spa"
  | "public-auth-rpc"
  | "worker-redeem-rpc"
  | "protected-rpc"
  | "worker-rpc"
  | "sync"
  | "worker"
  | "denied";

function atOrBelow(path: string, root: string): boolean {
  return path === root || path.startsWith(`${root}/`);
}

/** Header-independent managed-mode route classification. Method checks prevent
 * a path admitted for static GET from being reused as a POST sink. */
export function classifyManagedPublicRoute(
  path: string,
  method: string,
): ManagedPublicRouteKind {
  if (atOrBelow(path, "/internal") || atOrBelow(path, "/api")) {
    return "denied";
  }
  if (/^\/ws\/coord-worker\/[a-f0-9]{64}$/.test(path)) {
    return method === "GET" ? "worker" : "denied";
  }
  if (atOrBelow(path, "/ws/coord-worker")) return "denied";
  if (path === SYNC_WS_PATH) return method === "GET" ? "sync" : "denied";
  if (atOrBelow(path, "/ws")) return "denied";

  if (path.startsWith(COORDINATOR_RPC_PREFIX)) {
    const rpcMethod = path.slice(COORDINATOR_RPC_PREFIX.length);
    if (!rpcMethod || rpcMethod.includes("/")) return "denied";
    if (!REGISTERED_RPC_METHODS.has(rpcMethod)) return "denied";
    if (MANAGED_PRIVATE_RPC_METHODS[rpcMethod] === true) return "denied";
    if (method !== "POST" && method !== "OPTIONS") return "denied";
    if (rpcMethod === "AuthRedeemWorker") return "worker-redeem-rpc";
    if (MANAGED_WORKER_RPC_METHODS[rpcMethod]) return "worker-rpc";
    if (MANAGED_PUBLIC_AUTH_RPC_METHODS[rpcMethod] === true) return "public-auth-rpc";
    if (MANAGED_PROTECTED_RPC_METHODS[rpcMethod] === true) return "protected-rpc";
    return "denied";
  }

  if (path.startsWith("/roost.") || atOrBelow(path, "/roost")) return "denied";
  return method === "GET" || method === "HEAD" ? "spa" : "denied";
}
