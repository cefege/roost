// Owns Host and Origin admission for coordinator loopback listeners.
// Bun listener construction supplies the resolved port before this gate is created.
// It rejects DNS-rebinding requests before WebSocket upgrades or coordinator routes.
// Caller-address trust remains owned by caller-origin.ts and is intentionally separate.

import { DEFAULT_WORKER_LOCAL_UI_ORIGIN, type CoordConfig } from "@roost/host/config";
import { log } from "@roost/observability/log";

export type CoordinatorRequestAdmission = (request: Request) => Response | null;

export function createCoordinatorRequestAdmission(
  cfg: CoordConfig,
  boundPort: number,
): CoordinatorRequestAdmission {
  if (!/^127\.0\.0\.1:\d+$/.test(cfg.bind)) return () => null;

  const localAuthority = `127.0.0.1:${boundPort}`;
  const localOrigin = `http://${localAuthority}`;
  const allowedHostOrigins = new Set<string>([new URL(localOrigin).origin]);
  const allowedOrigins = new Set<string>([
    localOrigin,
    DEFAULT_WORKER_LOCAL_UI_ORIGIN,
    ...cfg.corsAllowedOrigins,
  ]);
  addDeclaredOrigin(cfg.webPublicUrl, allowedHostOrigins, allowedOrigins);
  addDeclaredOrigin(cfg.publicUrl, allowedHostOrigins, allowedOrigins);

  return (request) => {
    const hostOrigins = normalizedHostOrigins(request.headers.get("host"));
    if (!hostOrigins.some((origin) => allowedHostOrigins.has(origin))) return rejectAdmission("host");

    const origin = request.headers.get("origin");
    if (origin !== null && (origin === "null" || !allowedOrigins.has(origin))) {
      return rejectAdmission("origin");
    }
    return null;
  };
}

function addDeclaredOrigin(
  declaredOrigin: string | undefined,
  allowedHostOrigins: Set<string>,
  allowedOrigins: Set<string>,
): void {
  if (!declaredOrigin) return;
  const parsed = new URL(declaredOrigin);
  allowedHostOrigins.add(parsed.origin);
  allowedOrigins.add(parsed.origin);
}

function normalizedHostOrigins(rawHost: string | null): readonly string[] {
  if (
    rawHost === null
    || rawHost.length === 0
    || rawHost.trim() !== rawHost
    || /[\0-\x20\x7f\\/?#@]/.test(rawHost)
  ) return [];
  const origins: string[] = [];
  for (const protocol of ["http", "https"] as const) {
    try {
      const parsed = new URL(`${protocol}://${rawHost}`);
      if (
        parsed.username === ""
        && parsed.password === ""
        && parsed.pathname === "/"
        && parsed.search === ""
        && parsed.hash === ""
      ) origins.push(parsed.origin);
    } catch {
      // The other protocol cannot make malformed authority syntax valid.
    }
  }
  return origins;
}

function rejectAdmission(kind: "host" | "origin"): Response {
  log.warn("coord-admission", "request_rejected", { reason: kind });
  return new Response(`forbidden ${kind}`, { status: 403 });
}
