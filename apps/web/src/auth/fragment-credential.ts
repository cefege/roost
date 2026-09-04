// This module owns capture and scrubbing of authentication credentials carried in URL fragments.
// entry.ts calls it before transport, diagnostics, or error modules are allowed to evaluate.
// It depends only on browser-safe parsing, session storage, and strict tenant-route validation.
// Keeping this boundary narrow prevents secrets from leaking through startup requests or logs.

import { isTenantRouteKey } from "@roost/shared/tenant-route";
import { stageTenantRouteKeyFromCredential } from "./tenant-routing.ts";

export type CapturedFragmentCredential =
  | { kind: "pair"; token: string; routeKey?: string }
  | { kind: "relocation"; token: string; handoffId: string }
  | { kind: "activation"; token: string; routeKey: string }
  | { kind: "reset"; token: string; routeKey?: string }
  | {
      kind: "email-signup";
      token: string;
      expiresAtMs?: number;
      submittedAtMs?: number;
    };

export type CapturedFragmentCredentialKind = CapturedFragmentCredential["kind"];

export type FragmentCredential =
  | { kind: "none" }
  | { kind: "invalid" }
  | CapturedFragmentCredential;

interface CredentialUrl {
  pathname: string;
  search: string;
  hash: string;
  origin?: string;
}

interface CredentialStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

const CAPTURED_CREDENTIAL_KEY = "roost.fragmentCredential.v1";
export const EMAIL_SIGNUP_CREDENTIAL_TTL_MS = 7 * 24 * 60 * 60 * 1_000;
const RAW_TOKEN_RE = /^[A-Za-z0-9_-]{43}$/;
const GLOBAL_CREDENTIAL_KEYS: Readonly<Record<string, true | undefined>> = {
  pair: true,
  move: true,
  handoff: true,
};
let capturedCredential: CapturedFragmentCredential | null | undefined;

interface RouteCredentialPath {
  kind: "activation" | "reset";
  routeKey: string | null;
  cleanPath: "/activate" | "/reset-password";
}

function routeCredentialPath(pathname: string): RouteCredentialPath | null {
  const route = pathname === "/activate" || pathname.startsWith("/activate/")
    ? { kind: "activation" as const, cleanPath: "/activate" as const }
    : pathname === "/reset-password" || pathname.startsWith("/reset-password/")
      ? { kind: "reset" as const, cleanPath: "/reset-password" as const }
      : null;
  if (!route) return null;
  if (pathname === route.cleanPath) return { ...route, routeKey: null };
  const candidate = pathname.slice(route.cleanPath.length + 1);
  return { ...route, routeKey: isTenantRouteKey(candidate) ? candidate : null };
}
function pairRouteKey(pathname: string): string | null {
  if (!pathname.startsWith("/pair/")) return null;
  const candidate = pathname.slice("/pair/".length);
  return isTenantRouteKey(candidate) ? candidate : null;
}
function tenantNavigationPath(pathname: string): { routeKey: string; cleanPath: string } | null {
  const match = /^\/_roost\/t\/([0-9a-f]{64})(\/s\/[^/]+)$/.exec(pathname);
  return match?.[1] && match[2] ? { routeKey: match[1], cleanPath: match[2] } : null;
}
function isEmailSignupCredentialPath(pathname: string): boolean {
  return pathname === "/signup/verify" || pathname.startsWith("/signup/verify/");
}



function decodedParameterKey(segment: string): string | null {
  const separator = segment.indexOf("=");
  const encoded = separator === -1 ? segment : segment.slice(0, separator);
  try {
    return decodeURIComponent(encoded.replace(/\+/g, " "));
  } catch {
    return null;
  }
}

/** Remove selected query-style fields without normalizing unrelated bytes. */
function stripParameters(
  value: string,
  prefix: "?" | "#",
  keys: Readonly<Record<string, true | undefined>>,
): string {
  if (!value.startsWith(prefix)) return value;
  const segments = value.slice(1).split("&");
  let removed = false;
  const kept = segments.filter((segment) => {
    const key = decodedParameterKey(segment);
    const remove = key !== null && keys[key] === true;
    removed ||= remove;
    return !remove;
  });
  if (!removed) return value;
  return kept.length === 0 || (kept.length === 1 && kept[0] === "")
    ? ""
    : `${prefix}${kept.join("&")}`;
}

function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}
function isSafeTimestamp(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}

function storedCredential(value: unknown, nowMs = Date.now()): CapturedFragmentCredential | null {
  if (!value || typeof value !== "object" || !("kind" in value)) return null;
  if (
    value.kind === "pair"
    && "token" in value
    && isNonEmptyString(value.token)
  ) {
    return { kind: "pair", token: value.token };
  }
  if (
    (value.kind === "activation" || value.kind === "reset")
    && "token" in value
    && "routeKey" in value
    && isNonEmptyString(value.token)
    && isTenantRouteKey(value.routeKey)
  ) {
    return { kind: value.kind, token: value.token, routeKey: value.routeKey };
  }
  if (
    value.kind === "relocation"
    && "token" in value
    && "handoffId" in value
    && isNonEmptyString(value.token)
    && isNonEmptyString(value.handoffId)
  ) {
    return { kind: "relocation", token: value.token, handoffId: value.handoffId };
  }
  if (
    value.kind === "email-signup"
    && "token" in value
    && "expiresAtMs" in value
    && typeof value.token === "string"
    && RAW_TOKEN_RE.test(value.token)
    && isSafeTimestamp(value.expiresAtMs)
    && value.expiresAtMs > nowMs
    && value.expiresAtMs <= nowMs + EMAIL_SIGNUP_CREDENTIAL_TTL_MS
  ) {
    const submittedAtMs = "submittedAtMs" in value
      && isSafeTimestamp(value.submittedAtMs)
      && value.submittedAtMs <= nowMs
      ? value.submittedAtMs
      : undefined;
    return {
      kind: "email-signup",
      token: value.token,
      expiresAtMs: value.expiresAtMs,
      ...(submittedAtMs === undefined ? {} : { submittedAtMs }),
    };
  }
  return null;
}

function browserStorage(): CredentialStorage | null {
  try {
    return typeof sessionStorage === "undefined" ? null : sessionStorage;
  } catch {
    return null;
  }
}

function loadCapturedCredential(): CapturedFragmentCredential | null {
  const storage = browserStorage();
  if (!storage) return null;
  try {
    const raw = storage.getItem(CAPTURED_CREDENTIAL_KEY);
    if (raw === null) return null;
    const credential = storedCredential(JSON.parse(raw));
    if (credential) return credential;
    storage.removeItem(CAPTURED_CREDENTIAL_KEY);
  } catch {
    try { storage.removeItem(CAPTURED_CREDENTIAL_KEY); } catch { /* unavailable */ }
  }
  return null;
}

function retainCapturedCredential(credential: CapturedFragmentCredential): void {
  const retained = credential.kind === "email-signup"
    ? {
        ...credential,
        expiresAtMs: credential.expiresAtMs ?? Date.now() + EMAIL_SIGNUP_CREDENTIAL_TTL_MS,
      }
    : credential;
  capturedCredential = retained;
  try {
    browserStorage()?.setItem(CAPTURED_CREDENTIAL_KEY, JSON.stringify(retained));
  } catch {
    // Module memory still carries the credential for this document.
  }
}

function discardCapturedCredential(): void {
  capturedCredential = null;
  try {
    browserStorage()?.removeItem(CAPTURED_CREDENTIAL_KEY);
  } catch {
    // The in-memory copy is still gone.
  }
}

export function parseFragmentCredential(pathname: string, hash: string): FragmentCredential {
  if (isEmailSignupCredentialPath(pathname)) {
    const token = hash.startsWith("#") ? hash.slice(1) : hash;
    if (!token) return pathname === "/signup/verify" ? { kind: "none" } : { kind: "invalid" };
    return pathname === "/signup/verify" && RAW_TOKEN_RE.test(token)
      ? { kind: "email-signup", token }
      : { kind: "invalid" };
  }
  const pairKey = pairRouteKey(pathname);
  if (pathname.startsWith("/pair/") && pairKey === null) return { kind: "invalid" };
  const credentialRoute = routeCredentialPath(pathname);
  if (credentialRoute) {
    const token = hash.startsWith("#") ? hash.slice(1) : hash;
    if (!token) return credentialRoute.routeKey ? { kind: "invalid" } : { kind: "none" };
    if (RAW_TOKEN_RE.test(token)) {
      if (credentialRoute.routeKey) {
        return {
          kind: credentialRoute.kind,
          token,
          routeKey: credentialRoute.routeKey,
        };
      }
      if (credentialRoute.kind === "reset") return { kind: "reset", token };
    }
    return { kind: "invalid" };
  }

  const params = new URLSearchParams(hash.startsWith("#") ? hash.slice(1) : hash);
  const pairs = params.getAll("pair");
  const moves = params.getAll("move");
  const handoffs = params.getAll("handoff");
  const hasPair = params.has("pair");
  const hasMove = params.has("move");
  const hasHandoff = params.has("handoff");

  if (!hasPair && !hasMove && !hasHandoff) return { kind: "none" };
  if (
    hasPair
    && !hasMove
    && !hasHandoff
    && pairs.length === 1
    && isNonEmptyString(pairs[0])
  ) {
    return pairKey
      ? { kind: "pair", token: pairs[0], routeKey: pairKey }
      : { kind: "pair", token: pairs[0] };
  }
  if (
    !hasPair
    && hasMove
    && hasHandoff
    && moves.length === 1
    && handoffs.length === 1
    && isNonEmptyString(moves[0])
    && isNonEmptyString(handoffs[0])
  ) {
    return { kind: "relocation", token: moves[0], handoffId: handoffs[0] };
  }
  return { kind: "invalid" };
}

/**
 * Serialize an address without any credential-shaped query or fragment fields.
 * An origin is retained when supplied (diagnostics); history callers can omit it.
 */
export function credentialFreeUrl(url: CredentialUrl): string {
  const tenantNavigation = tenantNavigationPath(url.pathname);
  if (tenantNavigation) return `${url.origin ?? ""}${tenantNavigation.cleanPath}`;
  if (pairRouteKey(url.pathname)) return `${url.origin ?? ""}/`;
  if (isEmailSignupCredentialPath(url.pathname)) {
    return `${url.origin ?? ""}/signup/verify`;
  }
  const credentialRoute = routeCredentialPath(url.pathname);
  if (credentialRoute) return `${url.origin ?? ""}${credentialRoute.cleanPath}`;
  const search = stripParameters(url.search, "?", GLOBAL_CREDENTIAL_KEYS);
  const hash = stripParameters(url.hash, "#", GLOBAL_CREDENTIAL_KEYS);
  return `${url.origin ?? ""}${url.pathname}${search}${hash}`;
}

/**
 * Capture at most one fragment bearer and synchronously scrub the browser URL.
 * Query-shaped credentials are never accepted, but are also scrubbed before
 * entry.ts imports any module whose requests could serialize the current URL as
 * a Referer.
 */
export function captureAndScrubFragmentCredential(): FragmentCredential {
  if (typeof location === "undefined") return { kind: "none" };
  const current: CredentialUrl = {
    pathname: location.pathname,
    search: location.search,
    hash: location.hash,
  };
  const tenantNavigation = tenantNavigationPath(current.pathname);
  const credential = parseFragmentCredential(current.pathname, current.hash);
  const cleanUrl = credentialFreeUrl(current);
  const visibleUrl = `${current.pathname}${current.search}${current.hash}`;
  const containedCredentialData = cleanUrl !== visibleUrl;

  if (containedCredentialData) {
    // Do not catch this. If scrubbing fails, entry.ts must not continue into
    // network-facing code with a bearer still present in the document address.
    history.replaceState(null, "", cleanUrl);
  }
  if (
    tenantNavigation
    && !stageTenantRouteKeyFromCredential(tenantNavigation.routeKey)
  ) {
    throw new Error("Unable to persist the notification route");
  }
  if (
    (
      credential.kind === "activation"
      || credential.kind === "reset"
      || credential.kind === "pair"
    )
    && credential.routeKey !== undefined
    && !stageTenantRouteKeyFromCredential(credential.routeKey)
  ) {
    // The address is already credential-free, but continuing would make a
    // post-activation/reset reload fall back to another tenant.
    throw new Error("Unable to persist the account route");
  }

  if (
    credential.kind === "pair"
    || credential.kind === "relocation"
    || credential.kind === "activation"
    || credential.kind === "reset"
    || credential.kind === "email-signup"
  ) {
    retainCapturedCredential(credential);
  } else if (credential.kind === "invalid" || containedCredentialData) {
    // A new malformed/query-only attempt must never fall through to a stale
    // credential retained from an earlier document load.
    discardCapturedCredential();
  }
  return credential;
}
export function peekCapturedFragmentCredential(): CapturedFragmentCredential | null {
  if (capturedCredential === undefined) capturedCredential = loadCapturedCredential();
  if (
    capturedCredential?.kind === "email-signup"
    && (capturedCredential.expiresAtMs ?? 0) <= Date.now()
  ) {
    discardCapturedCredential();
  }
  return capturedCredential;
}
export function markCapturedEmailSignupSubmitted(nowMs = Date.now()): boolean {
  const current = peekCapturedFragmentCredential();
  if (current?.kind !== "email-signup") return false;
  retainCapturedCredential({ ...current, submittedAtMs: nowMs });
  return true;
}


/**
 * Clear only the credential the caller has just resolved. A stale async result
 * for another kind cannot discard a newer link captured in the same tab.
 */
export function clearCapturedFragmentCredential(
  expectedKind: CapturedFragmentCredentialKind,
): boolean {
  const current = peekCapturedFragmentCredential();
  if (current?.kind !== expectedKind) return false;
  discardCapturedCredential();
  return true;
}

/** Logout discards every pending proof, regardless of which managed flow captured it. */
export function clearCapturedFragmentCredentialsForLogout(): void {
  discardCapturedCredential();
}
