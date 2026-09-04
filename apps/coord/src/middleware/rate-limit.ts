// IP+endpoint sliding-window rate limiter. 100 req/min per IP per route group.
// Applied to Connect mutation routes that mint/mutate sensitive state:
// auth.*, workspaces.*, tasks.*, and MCP mutations.

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
  "/roost.v1.CoordinatorService/AuthMintBootstrap",
  "/roost.v1.CoordinatorService/AuthRedeemWorker",
  "/roost.v1.CoordinatorService/AuthRedeemBrowser",
  "/roost.v1.CoordinatorService/AuthPasswordLogin",
  "/roost.v1.CoordinatorService/AuthOwnerActivate",
  "/roost.v1.CoordinatorService/AuthPasswordResetRequest",
  "/roost.v1.CoordinatorService/AuthPasswordResetRedeem",
  "/roost.v1.CoordinatorService/AuthFederatedContinue",
  "/roost.v1.CoordinatorService/AuthPasswordAdd",
  "/roost.v1.CoordinatorService/AuthFederatedLinkBegin",
  "/roost.v1.CoordinatorService/AuthFederatedLink",
  "/roost.v1.CoordinatorService/AuthLogout",
  "/roost.v1.CoordinatorService/PairCreate",
  "/roost.v1.CoordinatorService/PairPoll",
  "/roost.v1.CoordinatorService/DevicesRevoke",
  "/roost.v1.CoordinatorService/DevicesRotateCurrent",
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
  // MCP mutations
  "/roost.v1.CoordinatorService/McpCreate",
  "/roost.v1.CoordinatorService/McpDelete",
  "/roost.v1.CoordinatorService/McpPublish",
  // worker control-plane mutations (rename + delete; Register +
  // Heartbeat run from workers themselves on a fixed cadence)
  "/roost.v1.CoordinatorService/WorkersRename",
  "/roost.v1.CoordinatorService/WorkersDelete",
  "/roost.v1.CoordinatorService/WorkersDeployStart",
  // transcription — Deepgram key write + stored-key handoff + test. GetConfig excluded.
  "/roost.v1.CoordinatorService/TranscriptionSetConfig",
  "/roost.v1.CoordinatorService/TranscriptionGrantToken",
  "/roost.v1.CoordinatorService/TranscriptionTest",
  // ui-cc — UiDispatch mutates live browser UI (mutation-class). ReportState
  // and ListStates stay unlimited: heartbeat/read-frequency traffic, same
  // reasoning as the *List exclusions above.
  "/roost.v1.CoordinatorService/UiDispatch",
]);

const TOKENS_PER_WINDOW = 100;
export const RATE_LIMIT_WINDOW_MS = 60_000;
export const RATE_LIMIT_MAX_BUCKETS = 10_000;

interface Bucket {
  remaining: number;
  resetAt: number;
  tokensPerWindow: number;
  windowMs: number;
  rejectionLogged: boolean;
}

export interface RateLimitDecision {
  allowed: boolean;
  retryAfterSeconds: number;
}

export interface RateLimiterOptions {
  now?: () => number;
  maxBuckets?: number;
  onReject?: (fields: {
    key: string;
    group: string;
    capacity: boolean;
  }) => void;
}

/**
 * Fixed-window limiter with bounded process state. Map insertion order is kept
 * in least-recently-used order so capacity maintenance examines cold entries
 * first. A full map of live buckets fails closed rather than evicting an active
 * limit and giving a churning caller a fresh budget.
 */
export class RateLimiter {
  private readonly buckets = new Map<string, Bucket>();
  private readonly now: () => number;
  private readonly maxBuckets: number;
  private readonly onReject: NonNullable<RateLimiterOptions["onReject"]>;
  private capacityLogResetAt = 0;

  constructor(options: RateLimiterOptions = {}) {
    this.now = options.now ?? Date.now;
    this.maxBuckets = options.maxBuckets ?? RATE_LIMIT_MAX_BUCKETS;
    if (!Number.isSafeInteger(this.maxBuckets) || this.maxBuckets <= 0) {
      throw new RangeError("maxBuckets must be a positive safe integer");
    }
    this.onReject = options.onReject ?? (() => {});
  }

  get bucketCount(): number {
    return this.buckets.size;
  }

  consume(
    key: string,
    group: string,
    tokensPerWindow: number,
    windowMs = RATE_LIMIT_WINDOW_MS,
  ): RateLimitDecision {
    if (!Number.isSafeInteger(tokensPerWindow) || tokensPerWindow <= 0) {
      throw new RangeError("tokensPerWindow must be a positive safe integer");
    }
    if (!Number.isSafeInteger(windowMs) || windowMs <= 0) {
      throw new RangeError("windowMs must be a positive safe integer");
    }

    const now = this.now();
    const bucketKey = `${group}\u0000${key}`;
    let bucket = this.buckets.get(bucketKey);
    if (bucket && (
      now >= bucket.resetAt
      || bucket.tokensPerWindow !== tokensPerWindow
      || bucket.windowMs !== windowMs
    )) {
      this.buckets.delete(bucketKey);
      bucket = undefined;
    }

    if (!bucket) {
      if (this.buckets.size >= this.maxBuckets) this.pruneExpired(now);
      if (this.buckets.size >= this.maxBuckets) {
        if (now >= this.capacityLogResetAt) {
          this.capacityLogResetAt = now + windowMs;
          this.onReject({ key: "capacity", group, capacity: true });
        }
        return {
          allowed: false,
          retryAfterSeconds: Math.max(1, Math.ceil(windowMs / 1_000)),
        };
      }
      bucket = {
        remaining: tokensPerWindow,
        resetAt: now + windowMs,
        tokensPerWindow,
        windowMs,
        rejectionLogged: false,
      };
      this.buckets.set(bucketKey, bucket);
    } else {
      // Refresh insertion order without extending the fixed rate window.
      this.buckets.delete(bucketKey);
      this.buckets.set(bucketKey, bucket);
    }

    if (bucket.remaining > 0) {
      bucket.remaining--;
      return { allowed: true, retryAfterSeconds: 0 };
    }
    if (!bucket.rejectionLogged) {
      bucket.rejectionLogged = true;
      this.onReject({ key, group, capacity: false });
    }
    return {
      allowed: false,
      retryAfterSeconds: Math.max(1, Math.ceil((bucket.resetAt - now) / 1_000)),
    };
  }

  response(
    key: string,
    group: string,
    tokensPerWindow: number,
    windowMs = RATE_LIMIT_WINDOW_MS,
  ): Response | null {
    const decision = this.consume(key, group, tokensPerWindow, windowMs);
    if (decision.allowed) return null;
    return new Response(
      JSON.stringify({ error: "rate limit exceeded" }),
      {
        status: 429,
        headers: {
          "content-type": "application/json",
          "retry-after": String(decision.retryAfterSeconds),
        },
      },
    );
  }

  private pruneExpired(now: number): void {
    for (const [key, bucket] of this.buckets) {
      if (now >= bucket.resetAt) this.buckets.delete(key);
    }
  }
}

const limiter = new RateLimiter({
  onReject: ({ key, group, capacity }) => {
    log.warn("rate-limit", "rate_limited", {
      scope_key: key,
      group,
      capacity,
    });
  },
});

function routeGroupKey(path: string): string | null {
  return RATE_LIMITED_ROUTES.has(path) ? path : null;
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
  return limiter.response(clientIp, group, TOKENS_PER_WINDOW);
}

/** Bounded fixed-window limit for public dispatch, WebSocket upgrades, and
 * credential sub-limits that do not share the generic Connect mutation rate. */
export function checkCustomLimit(
  key: string,
  group: string,
  tokensPerWindow: number,
  windowMs = RATE_LIMIT_WINDOW_MS,
): Response | null {
  return limiter.response(key, group, tokensPerWindow, windowMs);
}
