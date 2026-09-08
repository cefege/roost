// This module owns capture and scrubbing of authentication credentials carried in URL fragments.
// entry.ts calls it before transport, diagnostics, or error modules are allowed to evaluate.
// It depends only on browser-safe parsing and session storage, never on transport or store state.
// Keeping this boundary narrow prevents secrets from leaking through startup requests or logs.

export type CapturedFragmentCredential =
  | { kind: "pair"; token: string }
  | { kind: "relocation"; token: string; handoffId: string };

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
const GLOBAL_CREDENTIAL_KEYS: Readonly<Record<string, true | undefined>> = {
  pair: true,
  move: true,
  handoff: true,
};
let capturedCredential: CapturedFragmentCredential | null | undefined;

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

function storedCredential(value: unknown): CapturedFragmentCredential | null {
  if (!value || typeof value !== "object" || !("kind" in value)) return null;
  if (
    value.kind === "pair"
    && "token" in value
    && isNonEmptyString(value.token)
  ) {
    return { kind: "pair", token: value.token };
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
  capturedCredential = credential;
  try {
    browserStorage()?.setItem(CAPTURED_CREDENTIAL_KEY, JSON.stringify(credential));
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

export function parseFragmentCredential(hash: string): FragmentCredential {
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
    return { kind: "pair", token: pairs[0] };
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
  const credential = parseFragmentCredential(current.hash);
  const cleanUrl = credentialFreeUrl(current);
  const visibleUrl = `${current.pathname}${current.search}${current.hash}`;
  const containedCredentialData = cleanUrl !== visibleUrl;

  if (containedCredentialData) {
    // Do not catch this. If scrubbing fails, entry.ts must not continue into
    // network-facing code with a bearer still present in the document address.
    history.replaceState(null, "", cleanUrl);
  }

  if (credential.kind === "pair" || credential.kind === "relocation") {
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
  return capturedCredential;
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
