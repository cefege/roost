// This module owns non-authoritative tenant route hints and pending cross-tenant switch records.
// Entry, login, activation, and transport setup call it before choosing a coordinator URL.
// It depends on route-key validation plus browser storage, broadcast, and bounded fetch APIs.
// Separating routing from authorization prevents a persisted hint from becoming an authority claim.

import { isTenantRouteKey } from "@roost/shared/tenant-route";

export const TENANT_ROUTE_KEY_STORAGE_KEY = "roost.tenantRouteKey";
const TENANT_ROUTE_KEY_SESSION_FALLBACK = "roost.tenantRouteKey.session";
const TENANT_ROUTE_SWITCH_KEY = "roost.tenantRouteSwitch.v1";
const TENANT_ROUTE_SWITCH_BROADCAST_KEY = "roost.tenantRouteSwitch.broadcast.v1";
const TENANT_ROUTE_PREFIX = "/_roost/t";
const TENANT_RESOLVER_PATH = "/__roost/tenant/resolve";
const MAX_RESOLVER_RESPONSE_BYTES = 512;

const SYNCHRONOUS_AUTHORITY_HINT_KEYS = [
  "roostKeyMinted",
  "roostKeyAuthorized",
  "roost.dashboardId",
  "roost.syncLastEventId",
] as const;

interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface PendingTenantRouteSwitch {
  previousRouteKey: string | null;
  routeKey: string;
}

function browserLocalStorage(): StorageLike | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

function browserSessionStorage(): StorageLike | null {
  try {
    return typeof sessionStorage === "undefined" ? null : sessionStorage;
  } catch {
    return null;
  }
}

function readValidatedRouteKey(storage: StorageLike | null, key: string): string | null {
  if (!storage) return null;
  try {
    const value = storage.getItem(key);
    if (value === null || isTenantRouteKey(value)) return value;
    storage.removeItem(key);
  } catch {
    // Storage is optional; the caller can still use an explicitly resolved key.
  }
  return null;
}

export function storedTenantRouteKey(): string | null {
  return readValidatedRouteKey(browserLocalStorage(), TENANT_ROUTE_KEY_STORAGE_KEY)
    ?? readValidatedRouteKey(browserSessionStorage(), TENANT_ROUTE_KEY_SESSION_FALLBACK);
}

export function tenantRoutePrefix(routeKey: string): string {
  if (!isTenantRouteKey(routeKey)) throw new TypeError("invalid tenant route key");
  return `${TENANT_ROUTE_PREFIX}/${routeKey}`;
}

function browserOrigin(): string {
  return typeof location === "undefined" ? "http://localhost" : location.origin;
}

export function tenantCoordinatorBaseUrl(routeKey: string, origin = browserOrigin()): string {
  const normalizedOrigin = origin.replace(/\/+$/, "");
  return `${normalizedOrigin}${tenantRoutePrefix(routeKey)}`;
}

/** Persist a routing hint, never an authority claim. localStorage keeps the
 * selection across browser restarts; sessionStorage is a private-mode fallback. */
export function persistTenantRouteKey(routeKey: string): boolean {
  if (!isTenantRouteKey(routeKey)) return false;
  const local = browserLocalStorage();
  if (local) {
    try {
      local.setItem(TENANT_ROUTE_KEY_STORAGE_KEY, routeKey);
      try { browserSessionStorage()?.removeItem(TENANT_ROUTE_KEY_SESSION_FALLBACK); } catch { /* unavailable */ }
      return true;
    } catch {
      // Fall through to a tab-lifetime selection.
    }
  }
  try {
    const session = browserSessionStorage();
    if (!session) return false;
    session.setItem(TENANT_ROUTE_KEY_SESSION_FALLBACK, routeKey);
    return true;
  } catch {
    return false;
  }
}
export function commitTenantRouteKey(routeKey: string): boolean {
  if (!persistTenantRouteKey(routeKey)) return false;
  broadcastTenantRouteSwitch();
  return true;
}

export function broadcastTenantRouteSwitch(): void {
  const storage = browserLocalStorage();
  if (!storage) return;
  try {
    const nonce = typeof crypto !== "undefined" && typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : `${Date.now()}-${Math.random()}`;
    storage.setItem(TENANT_ROUTE_SWITCH_BROADCAST_KEY, nonce);
    storage.removeItem(TENANT_ROUTE_SWITCH_BROADCAST_KEY);
  } catch {
    // Current-tab invalidation still closes every local transport.
  }
}

export function installTenantRouteSwitchListener(onSwitch: () => void): () => void {
  if (typeof window === "undefined") return () => {};
  const listener = (event: StorageEvent): void => {
    if (event.key === TENANT_ROUTE_SWITCH_BROADCAST_KEY && event.newValue !== null) onSwitch();
  };
  window.addEventListener("storage", listener);
  return () => window.removeEventListener("storage", listener);
}


function clearSynchronousAuthorityHints(): void {
  const local = browserLocalStorage();
  if (!local) return;
  for (const key of SYNCHRONOUS_AUTHORITY_HINT_KEYS) {
    try { local.removeItem(key); } catch { /* unavailable */ }
  }
}

function writePendingTenantRouteSwitch(value: PendingTenantRouteSwitch): void {
  const serialized = JSON.stringify(value);
  for (const storage of [browserLocalStorage(), browserSessionStorage()]) {
    if (!storage) continue;
    try {
      storage.setItem(TENANT_ROUTE_SWITCH_KEY, serialized);
      return;
    } catch {
      // Try the other browser storage.
    }
  }
}

/** Called only by the synchronous credential-entry boundary. It records the
 * previous route before selecting the link's route and removes enrollment
 * hints before web-key.ts can snapshot them. The full IndexedDB/Push cleanup
 * runs before the application is mounted. */
export function stageTenantRouteKeyFromCredential(routeKey: string): boolean {
  if (!isTenantRouteKey(routeKey)) return false;
  const current = storedTenantRouteKey();
  if (current === routeKey) return true;

  const alreadyPending = pendingTenantRouteSwitch();
  writePendingTenantRouteSwitch({
    previousRouteKey: alreadyPending?.previousRouteKey ?? current,
    routeKey,
  });
  clearSynchronousAuthorityHints();
  return persistTenantRouteKey(routeKey);
}

export function pendingTenantRouteSwitch(): PendingTenantRouteSwitch | null {
  for (const storage of [browserLocalStorage(), browserSessionStorage()]) {
    if (!storage) continue;
    try {
      const raw = storage.getItem(TENANT_ROUTE_SWITCH_KEY);
      if (raw === null) continue;
      const parsed = JSON.parse(raw) as Partial<PendingTenantRouteSwitch> | null;
      if (
        parsed
        && isTenantRouteKey(parsed.routeKey)
        && (parsed.previousRouteKey === null || isTenantRouteKey(parsed.previousRouteKey))
      ) {
        return {
          previousRouteKey: parsed.previousRouteKey,
          routeKey: parsed.routeKey,
        };
      }
      storage.removeItem(TENANT_ROUTE_SWITCH_KEY);
    } catch {
      try { storage.removeItem(TENANT_ROUTE_SWITCH_KEY); } catch { /* unavailable */ }
    }
  }
  return null;
}

export function clearPendingTenantRouteSwitch(expectedRouteKey: string): void {
  for (const storage of [browserLocalStorage(), browserSessionStorage()]) {
    if (!storage) continue;
    try {
      const raw = storage.getItem(TENANT_ROUTE_SWITCH_KEY);
      if (raw === null) continue;
      const parsed = JSON.parse(raw) as Partial<PendingTenantRouteSwitch> | null;
      if (parsed?.routeKey === expectedRouteKey) storage.removeItem(TENANT_ROUTE_SWITCH_KEY);
    } catch {
      try { storage.removeItem(TENANT_ROUTE_SWITCH_KEY); } catch { /* unavailable */ }
    }
  }
}

/** Explicit logout abandons any interrupted credential-driven route switch.
 * The durable route hint remains non-authoritative and is intentionally kept. */
export function clearPendingTenantRouteSwitchForLogout(): void {
  for (const storage of [browserLocalStorage(), browserSessionStorage()]) {
    try { storage?.removeItem(TENANT_ROUTE_SWITCH_KEY); } catch { /* unavailable */ }
  }
}

function resolverError(): TypeError {
  return new TypeError("Roost couldn’t resolve that account route");
}

/** Resolve an email to a non-authoritative opaque route. The no-referrer POST
 * deliberately targets the unprefixed shared resolver and accepts only the
 * fixed-size response contract. */
export async function resolveTenantRouteKey(
  email: string,
  fetchImpl: typeof fetch = globalThis.fetch,
): Promise<string> {
  let response: Response;
  try {
    response = await fetchImpl(`${browserOrigin()}${TENANT_RESOLVER_PATH}`, {
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ email: email.trim() }),
      cache: "no-store",
      credentials: "same-origin",
      redirect: "error",
      referrerPolicy: "no-referrer",
    });
  } catch {
    throw resolverError();
  }
  if (!response.ok) throw resolverError();

  let body: string;
  try {
    body = await response.text();
  } catch {
    throw resolverError();
  }
  if (new TextEncoder().encode(body).byteLength > MAX_RESOLVER_RESPONSE_BYTES) {
    throw resolverError();
  }

  try {
    const value = JSON.parse(body) as { routeKey?: unknown };
    if (!isTenantRouteKey(value.routeKey)) throw resolverError();
    return value.routeKey;
  } catch (error) {
    if (error instanceof TypeError) throw error;
    throw resolverError();
  }
}
