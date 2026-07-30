// IP+endpoint sliding-window rate limiter. 100 req/min per IP per route group.
// Applied to Connect mutation routes that mint/mutate sensitive state:
// auth.*, workspaces.*, tasks.enqueue, webhookTokens.mint.

import { log } from "@roost/shared/log";

// Routes subject to rate limiting. List by exact RPC name (NOT prefix)
// so read-only operations sharing a prefix don't burn the mutation
// budget. Connect always uses POST so the request.method GET-bypass at
// the bottom of this file doesn't help on the protocol level; we must
// exclude `*List` / `*Read` by name. Prior shape used prefix
// `/roost.v1.CoordinatorService/Workspaces` which matched WorkspacesList
// (called on every SPA bootstrap + visibilitychange focus refresh),
// eating the same 100/min bucket as create/update/delete mutations.
const RATE_LIMITED_ROUTES: ReadonlySet<string> = new Set([
  // auth mutations — credential issue / consumption surfaces
  "/roost.v1.CoordinatorService/AuthAuthorizeBrowser",
  "/roost.v1.CoordinatorService/AuthMintBootstrap",
  "/roost.v1.CoordinatorService/AuthRedeemWorker",
  "/roost.v1.CoordinatorService/AuthRedeemBrowser",
  // workspace mutations (List read excluded — bootstrap + focus refresh)
  "/roost.v1.CoordinatorService/WorkspacesCreate",
  "/roost.v1.CoordinatorService/WorkspacesUpdate",
  "/roost.v1.CoordinatorService/WorkspacesDelete",
  "/roost.v1.CoordinatorService/WorkspacesSetSessions",
  // task mutations (NextPending allowed unbounded — workers poll on a
  // backoff schedule; Enqueue/SetState/Cancel are the user-driven
  // surfaces that need throttling)
  "/roost.v1.CoordinatorService/TasksEnqueue",
  "/roost.v1.CoordinatorService/TasksSetState",
  "/roost.v1.CoordinatorService/TasksCancel",
  // webhook + permission + mcp mutations
  "/roost.v1.CoordinatorService/WebhookTokensMint",
  "/roost.v1.CoordinatorService/WebhookTokensDelete",
  "/roost.v1.CoordinatorService/PermissionsCreate",
  "/roost.v1.CoordinatorService/PermissionsUpdate",
  "/roost.v1.CoordinatorService/PermissionsDelete",
  "/roost.v1.CoordinatorService/McpCreate",
  "/roost.v1.CoordinatorService/McpDelete",
  "/roost.v1.CoordinatorService/McpPublish",
  // worker control-plane mutations (rename + delete; Register +
  // Heartbeat run from workers themselves on a fixed cadence)
  "/roost.v1.CoordinatorService/WorkersRename",
  "/roost.v1.CoordinatorService/WorkersDelete",
  "/roost.v1.CoordinatorService/WorkersDeployStart",
  // transcription — Deepgram key write + token grant + test. GetConfig excluded.
  "/roost.v1.CoordinatorService/TranscriptionSetConfig",
  "/roost.v1.CoordinatorService/TranscriptionGrantToken",
  "/roost.v1.CoordinatorService/TranscriptionTest",
  // ui-cc — UiDispatch mutates live browser UI (mutation-class). ReportState
  // and ListStates stay unlimited: heartbeat/read-frequency traffic, same
  // reasoning as the *List exclusions above.
  "/roost.v1.CoordinatorService/UiDispatch",
]);

const TOKENS_PER_WINDOW = 100;
const WINDOW_MS = 60_000;

interface Bucket {
  tokens: number;
  lastRefill: number;
}

const buckets = new Map<string, Bucket>();

function routeGroupKey(path: string): string | null {
  return RATE_LIMITED_ROUTES.has(path) ? path : null;
}

function checkAndConsume(ip: string, group: string): boolean {
  const key = `${ip}|${group}`;
  const now = Date.now();
  let bucket = buckets.get(key);
  if (!bucket) {
    bucket = { tokens: TOKENS_PER_WINDOW, lastRefill: now };
    buckets.set(key, bucket);
  }
  const elapsed = now - bucket.lastRefill;
  if (elapsed >= WINDOW_MS) {
    bucket.tokens = TOKENS_PER_WINDOW;
    bucket.lastRefill = now;
  } else if (elapsed > 0) {
    const refill = Math.floor((elapsed / WINDOW_MS) * TOKENS_PER_WINDOW);
    if (refill > 0) {
      bucket.tokens = Math.min(TOKENS_PER_WINDOW, bucket.tokens + refill);
      bucket.lastRefill = now;
    }
  }
  if (bucket.tokens <= 0) return false;
  bucket.tokens--;
  return true;
}

let lastPrune = Date.now();
function maybePrune(): void {
  const now = Date.now();
  if (now - lastPrune < WINDOW_MS * 2) return;
  lastPrune = now;
  for (const [key, bucket] of buckets) {
    if (now - bucket.lastRefill > WINDOW_MS * 2) buckets.delete(key);
  }
}

/**
 * Check rate limit for an incoming request. Returns null when the
 * request is allowed; returns a 429 Response when it should be rejected.
 */
export function checkRateLimit(req: Request, clientIp: string): Response | null {
  if (req.method === "GET" || req.method === "HEAD" || req.method === "OPTIONS") {
    return null;
  }
  const path = new URL(req.url).pathname;
  const group = routeGroupKey(path);
  if (!group) return null;

  maybePrune();
  const allowed = checkAndConsume(clientIp, group);
  if (allowed) return null;

  log.warn("rate-limit", "rate_limited", { ip: clientIp, group, path });
  return new Response(
    JSON.stringify({ error: "rate limit exceeded — 100 req/min per IP" }),
    {
      status: 429,
      headers: {
        "content-type": "application/json",
        "retry-after": "60",
      },
    },
  );
}
