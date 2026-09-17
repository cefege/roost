// Which worker door, if any, can this page reach on the browser's own machine.
// A page a worker served knows the answer from its own origin; a page the
// coordinator served asks the machine's default loopback door once, the first
// time a terminal pane goes live. ws/local-terminal.ts dials the answer.
// Deliberately separate from localBootstrap.ts, which answers the different
// question "was this page served BY a worker" — connect.ts routes every
// coordinator RPC off that fact, and a discovered door must never move them.
// The door's advertised coordinatorUrl is ignored: the coordinator-minted grant
// is what authorizes the socket, and a hardened deployment legitimately has the
// worker dialing a different URL than the browser's front door.

import { diag } from "@roost/shared/diag";
import { DEFAULT_WORKER_LOCAL_UI_ORIGIN } from "@roost/shared/local-ui-door";
import {
  LOCAL_BOOTSTRAP_PATH,
  parseLocalBootstrapPayload,
  readLocalBootstrap,
} from "./localBootstrap.ts";

export interface LocalWorkerDoor {
  readonly origin: string;
  readonly workerFingerprint: string;
}

/** Operator escape hatch for a door on a non-default port. Nothing reports a
 * worker's local-UI port to the coordinator, so there is nothing to derive. */
export const LOCAL_WORKER_ORIGIN_KEY = "roost.localWorkerOrigin";

const PROBE_TIMEOUT_MS = 2_000;

let door: LocalWorkerDoor | null = null;
let attempted = false;
let handlers: ((door: LocalWorkerDoor) => void)[] = [];

/** The reachable local worker door, or null while none is known. */
export function readLocalWorkerDoor(): LocalWorkerDoor | null {
  return door;
}

/** Memoized one-shot. Never throws and never returns a promise: the caller is
 * on a terminal pane's publish path and must not wait on a network probe. */
export function discoverLocalWorkerDoor(): void {
  if (attempted) return;
  attempted = true;
  const bootstrap = readLocalBootstrap();
  if (bootstrap) {
    adopt({
      origin: location.origin,
      workerFingerprint: bootstrap.workerFingerprint,
    });
    return;
  }
  void probe();
}

export function registerLocalWorkerDoorHandler(
  handler: (door: LocalWorkerDoor) => void,
): void {
  handlers.push(handler);
}

export function _resetLocalWorkerDiscoveryForTest(): void {
  door = null;
  attempted = false;
  handlers = [];
}

async function probe(): Promise<void> {
  const origin = candidateOrigin();
  // The coordinator already answered 404 for this path during startup; asking
  // its origin again would only burn a request.
  if (typeof location !== "undefined" && origin === location.origin) return;
  try {
    const response = await fetch(`${origin}${LOCAL_BOOTSTRAP_PATH}`, {
      method: "GET",
      mode: "cors",
      cache: "no-store",
      credentials: "omit",
      signal: AbortSignal.timeout(PROBE_TIMEOUT_MS),
    });
    if (!response.ok) {
      diag("local_terminal.door_absent", { origin, error: `status ${response.status}` });
      return;
    }
    const answer = parseLocalBootstrapPayload(await response.json());
    if (!answer) {
      diag("local_terminal.door_absent", { origin, error: "unusable body" });
      return;
    }
    adopt({ origin, workerFingerprint: answer.workerFingerprint });
    diag("local_terminal.door_discovered", {
      origin,
      worker_fp: answer.workerFingerprint,
    });
  } catch (error) {
    diag("local_terminal.door_absent", { origin, error: String(error) });
  }
}

function adopt(next: LocalWorkerDoor): void {
  door = next;
  for (const handler of handlers) handler(next);
}

function candidateOrigin(): string {
  if (typeof localStorage === "undefined") return DEFAULT_WORKER_LOCAL_UI_ORIGIN;
  const stored = localStorage.getItem(LOCAL_WORKER_ORIGIN_KEY);
  if (!stored) return DEFAULT_WORKER_LOCAL_UI_ORIGIN;
  try {
    const url = new URL(stored);
    const bare = url.origin === stored;
    if (bare && (url.protocol === "http:" || url.protocol === "https:")) return stored;
  } catch {
    // Fall through: a malformed override is ignored rather than dialed.
  }
  return DEFAULT_WORKER_LOCAL_UI_ORIGIN;
}
