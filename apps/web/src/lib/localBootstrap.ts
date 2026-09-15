// The worker serves this same SPA on its loopback origin so a browser on the
// PTY's own machine can talk to that worker directly. This module asks the
// serving origin who it is, once, before the SPA graph loads; connect.ts reads
// the answer synchronously at module scope. Dependency-free (fetch + JSON) and
// fail-closed: anything unexpected leaves the page on its normal coordinator
// path instead of throwing during startup.

export interface LocalBootstrap {
  coordinatorUrl: string;
  workerFingerprint: string;
}

const BOOTSTRAP_PATH = "/api/local-bootstrap";
const BOOTSTRAP_TIMEOUT_MS = 2_000;

let bootstrap: LocalBootstrap | null = null;

/** Probes the serving origin for a worker-served bootstrap. Never throws and
 * never blocks startup for longer than BOOTSTRAP_TIMEOUT_MS. */
export async function loadLocalBootstrap(): Promise<void> {
  bootstrap = await fetchLocalBootstrap();
}

/** The worker-served bootstrap, or null when the coordinator served this page. */
export function readLocalBootstrap(): LocalBootstrap | null {
  return bootstrap;
}

async function fetchLocalBootstrap(): Promise<LocalBootstrap | null> {
  try {
    // Same-origin and relative on purpose: only the worker's local UI server
    // answers this path, and the coordinator's 404 is the "not worker-served"
    // answer. Bounded so a hung serving origin cannot stall SPA startup.
    const response = await fetch(BOOTSTRAP_PATH, {
      method: "GET",
      cache: "no-store",
      credentials: "omit",
      signal: AbortSignal.timeout(BOOTSTRAP_TIMEOUT_MS),
    });
    if (!response.ok) return null;
    return parseLocalBootstrap(await response.json());
  } catch {
    return null;
  }
}

function parseLocalBootstrap(payload: unknown): LocalBootstrap | null {
  if (typeof payload !== "object" || payload === null) return null;
  const fields = payload as { coordinatorUrl?: unknown; workerFingerprint?: unknown };
  if (typeof fields.coordinatorUrl !== "string" || typeof fields.workerFingerprint !== "string") {
    return null;
  }
  const coordinatorUrl = fields.coordinatorUrl.trim();
  const workerFingerprint = fields.workerFingerprint.trim();
  if (!coordinatorUrl || !workerFingerprint) return null;
  // A relative or non-HTTP URL would silently retarget every coordinator RPC
  // at this page's own origin, which is the worker — refuse it instead.
  try {
    const protocol = new URL(coordinatorUrl).protocol;
    if (protocol !== "https:" && protocol !== "http:") return null;
  } catch {
    return null;
  }
  return { coordinatorUrl, workerFingerprint };
}
