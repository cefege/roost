// This module owns credential changes for an already authenticated managed account.
// Account settings call it to inspect sign-in methods, link Google, or add a password.
// It depends on tenant routing, the WebCrypto identity, coordinator RPCs, and bounded gateway calls.
// Rechecking the actor prevents asynchronous changes from crossing tenant or browser-key boundaries.

import { isTenantRouteKey } from "@roost/shared/tenant-route";
import { makeCoordinatorClientForSigner } from "../connect.ts";
import {
  ManagedNewPasswordError,
  managedNewPasswordIssue,
} from "./managed-account.ts";
import {
  storedTenantRouteKey,
  tenantCoordinatorBaseUrl,
} from "./tenant-routing.ts";
import {
  getCurrentWebKeyInfo,
  signCoordinatorJwt,
} from "./web-key.ts";

const GOOGLE_AUTHORIZATION_ORIGIN = "https://accounts.google.com";
const GOOGLE_AUTHORIZATION_PATH = "/o/oauth2/v2/auth";
const GATEWAY_BODY_LIMIT = 16 * 1024;
const LINK_COMPLETION_TIMEOUT_MS = 10 * 60 * 1_000;
const HASH_RE = /^[0-9a-f]{64}$/u;
const COMPACT_JWT_RE = /^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/u;

export const MANAGED_GOOGLE_IDENTITY_UNAVAILABLE_MESSAGE = "Google identity unavailable.";
export const MANAGED_CREDENTIALS_UNAVAILABLE_MESSAGE =
  "Sign-in methods are unavailable right now. Try again.";
export const MANAGED_PASSWORD_ADD_FAILED_MESSAGE =
  "Roost couldn’t add a password. Try again.";

export interface ManagedCredentials {
  email: string;
  hasPassword: boolean;
  googleLinked: boolean;
}

export type ManagedAccountBusyAction = "devices" | "google" | "password" | "logout";

/** One mutation at a time across credential, device, and session controls. */
export class ManagedAccountBusyGate {
  #current: ManagedAccountBusyAction | null = null;

  get current(): ManagedAccountBusyAction | null {
    return this.#current;
  }

  begin(action: ManagedAccountBusyAction): boolean {
    if (this.#current !== null) return false;
    this.#current = action;
    return true;
  }

  finish(action: ManagedAccountBusyAction): boolean {
    if (this.#current !== action) return false;
    this.#current = null;
    return true;
  }
}

interface ManagedCredentialClient {
  authCredentialsGet(request: Record<string, never>): Promise<{
    email: string;
    hasPassword: boolean;
    googleLinked: boolean;
  }>;
  authPasswordAdd(request: { newPassword: string }): Promise<{ ok: boolean }>;
  authFederatedLinkBegin(request: Record<string, never>): Promise<{ linkTicket: string }>;
  authFederatedLink(request: { assertion: string }): Promise<{ ok: boolean }>;
}

interface ManagedWebKeyInfo {
  fingerprint: string;
  extractable: boolean;
}

export interface ManagedCredentialsDependencies {
  storedRouteKey: () => string | null;
  currentWebKeyInfo: () => Promise<ManagedWebKeyInfo>;
  clientForRoute: (routeKey: string) => ManagedCredentialClient;
  fetch: typeof globalThis.fetch;
  now: () => number;
  sleep: (delayMs: number) => Promise<void>;
}

function defaultDependencies(): ManagedCredentialsDependencies {
  return {
    storedRouteKey: storedTenantRouteKey,
    currentWebKeyInfo: getCurrentWebKeyInfo,
    clientForRoute: (routeKey) => makeCoordinatorClientForSigner(
      signCoordinatorJwt,
      tenantCoordinatorBaseUrl(routeKey),
    ),
    fetch: globalThis.fetch,
    now: Date.now,
    sleep: (delayMs) => {
      const { promise, resolve } = Promise.withResolvers<void>();
      setTimeout(resolve, delayMs);
      return promise;
    },
  };
}

export class ManagedGoogleIdentityUnavailableError extends Error {
  constructor() {
    super(MANAGED_GOOGLE_IDENTITY_UNAVAILABLE_MESSAGE);
    this.name = "ManagedGoogleIdentityUnavailableError";
  }
}

export class ManagedCredentialBindingError extends Error {
  constructor() {
    super("managed credential actor binding changed");
    this.name = "ManagedCredentialBindingError";
  }
}

interface ManagedActorBinding {
  routeKey: string;
  fingerprint: string;
}

function exactRouteKey(value: string | null): string {
  if (!value || !isTenantRouteKey(value)) throw new ManagedCredentialBindingError();
  return value;
}

function exactFingerprint(info: ManagedWebKeyInfo): string {
  if (info.extractable || !HASH_RE.test(info.fingerprint)) {
    throw new ManagedCredentialBindingError();
  }
  return info.fingerprint;
}

async function currentActor(
  dependencies: ManagedCredentialsDependencies,
): Promise<ManagedActorBinding> {
  const routeKey = exactRouteKey(dependencies.storedRouteKey());
  const fingerprint = exactFingerprint(await dependencies.currentWebKeyInfo());
  if (dependencies.storedRouteKey() !== routeKey) throw new ManagedCredentialBindingError();
  return { routeKey, fingerprint };
}

async function requireSameActor(
  expected: ManagedActorBinding,
  dependencies: ManagedCredentialsDependencies,
): Promise<void> {
  if (dependencies.storedRouteKey() !== expected.routeKey) {
    throw new ManagedCredentialBindingError();
  }
  const fingerprint = exactFingerprint(await dependencies.currentWebKeyInfo());
  if (fingerprint !== expected.fingerprint || dependencies.storedRouteKey() !== expected.routeKey) {
    throw new ManagedCredentialBindingError();
  }
}

function exactCredentials(value: {
  email: string;
  hasPassword: boolean;
  googleLinked: boolean;
}): ManagedCredentials {
  if (
    typeof value.email !== "string"
    || value.email.length < 3
    || value.email.length > 320
    || value.email !== value.email.trim()
    || typeof value.hasPassword !== "boolean"
    || typeof value.googleLinked !== "boolean"
  ) throw new TypeError("invalid managed credential response");
  return {
    email: value.email,
    hasPassword: value.hasPassword,
    googleLinked: value.googleLinked,
  };
}

async function readGatewayObject(response: Response): Promise<Record<string, unknown>> {
  const text = await response.text();
  if (new TextEncoder().encode(text).byteLength > GATEWAY_BODY_LIMIT) {
    throw new ManagedGoogleIdentityUnavailableError();
  }
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw new ManagedGoogleIdentityUnavailableError();
  }
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new ManagedGoogleIdentityUnavailableError();
  }
  return value as Record<string, unknown>;
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  return actual.length === expected.length && actual.every((key, index) => key === expected[index]);
}

function gatewayPost(fetchImpl: typeof globalThis.fetch, path: string, body: string): Promise<Response> {
  return fetchImpl(path, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/json",
    },
    body,
    cache: "no-store",
    credentials: "same-origin",
    redirect: "error",
    referrerPolicy: "no-referrer",
  });
}

function authorizationUrl(value: unknown): string {
  if (typeof value !== "string" || value.length > 4_096) {
    throw new ManagedGoogleIdentityUnavailableError();
  }
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new ManagedGoogleIdentityUnavailableError();
  }
  if (
    url.origin !== GOOGLE_AUTHORIZATION_ORIGIN
    || url.pathname !== GOOGLE_AUTHORIZATION_PATH
    || url.username !== ""
    || url.password !== ""
    || url.hash !== ""
  ) throw new ManagedGoogleIdentityUnavailableError();
  return url.href;
}

function assertionBinding(assertion: string): {
  purpose: unknown;
  routeKey: unknown;
  fingerprint: unknown;
} {
  if (typeof assertion !== "string" || assertion.length > GATEWAY_BODY_LIMIT || !COMPACT_JWT_RE.test(assertion)) {
    throw new ManagedCredentialBindingError();
  }
  const payload = assertion.split(".")[1]!;
  let decoded: string;
  try {
    const padded = payload.replace(/-/gu, "+").replace(/_/gu, "/")
      + "=".repeat((4 - (payload.length % 4)) % 4);
    const bytes = Uint8Array.from(atob(padded), (character) => character.charCodeAt(0));
    decoded = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new ManagedCredentialBindingError();
  }
  let claims: unknown;
  try {
    claims = JSON.parse(decoded);
  } catch {
    throw new ManagedCredentialBindingError();
  }
  if (claims === null || typeof claims !== "object" || Array.isArray(claims)) {
    throw new ManagedCredentialBindingError();
  }
  const record = claims as Record<string, unknown>;
  return {
    purpose: record.purpose,
    routeKey: record.route_key,
    fingerprint: record.device_fp,
  };
}

/** Dispatch hint only. The coordinator remains the signature/claim authority. */
export function isManagedGoogleLinkAssertion(assertion: string): boolean {
  try {
    return assertionBinding(assertion).purpose === "link";
  } catch {
    return false;
  }
}

export async function getManagedCredentials(
  dependencies: ManagedCredentialsDependencies = defaultDependencies(),
): Promise<ManagedCredentials> {
  const actor = await currentActor(dependencies);
  const client = dependencies.clientForRoute(actor.routeKey);
  const credentials = exactCredentials(await client.authCredentialsGet({}));
  await requireSameActor(actor, dependencies);
  return credentials;
}

export async function beginManagedGoogleLink(
  dependencies: ManagedCredentialsDependencies = defaultDependencies(),
): Promise<{ authorizationUrl: string }> {
  const actor = await currentActor(dependencies);
  const client = dependencies.clientForRoute(actor.routeKey);
  const { linkTicket } = await client.authFederatedLinkBegin({});
  if (
    typeof linkTicket !== "string"
    || linkTicket.length > 8_192
    || !COMPACT_JWT_RE.test(linkTicket)
  ) throw new ManagedGoogleIdentityUnavailableError();
  await requireSameActor(actor, dependencies);

  let response: Response;
  try {
    response = await gatewayPost(dependencies.fetch, "/__roost/auth/google/start", JSON.stringify({
      intent: "link",
      routeKey: actor.routeKey,
      linkTicket,
    }));
  } catch {
    throw new ManagedGoogleIdentityUnavailableError();
  }
  const result = await readGatewayObject(response);
  if (!response.ok || !hasExactKeys(result, ["authorizationUrl"])) {
    throw new ManagedGoogleIdentityUnavailableError();
  }
  const url = authorizationUrl(result.authorizationUrl);
  await requireSameActor(actor, dependencies);
  return { authorizationUrl: url };
}

export async function completeManagedGoogleLink(
  input: { routeKey: string; assertion: string },
  dependencies: ManagedCredentialsDependencies = defaultDependencies(),
): Promise<ManagedCredentials> {
  if (!isTenantRouteKey(input.routeKey)) throw new ManagedCredentialBindingError();
  const actor = await currentActor(dependencies);
  if (actor.routeKey !== input.routeKey) throw new ManagedCredentialBindingError();
  const binding = assertionBinding(input.assertion);
  if (
    binding.purpose !== "link"
    || binding.routeKey !== actor.routeKey
    || binding.fingerprint !== actor.fingerprint
  ) throw new ManagedCredentialBindingError();

  const client = dependencies.clientForRoute(actor.routeKey);
  const linked = await client.authFederatedLink({ assertion: input.assertion });
  if (linked.ok !== true) throw new ManagedGoogleIdentityUnavailableError();
  await requireSameActor(actor, dependencies);

  const deadline = dependencies.now() + LINK_COMPLETION_TIMEOUT_MS;
  while (dependencies.now() < deadline) {
    await requireSameActor(actor, dependencies);
    let response: Response;
    try {
      response = await gatewayPost(dependencies.fetch, "/__roost/auth/link/complete", "{}");
    } catch {
      await dependencies.sleep(1_000);
      continue;
    }
    const result = await readGatewayObject(response);
    if (response.ok && hasExactKeys(result, ["state"]) && result.state === "ready") {
      await requireSameActor(actor, dependencies);
      const credentials = exactCredentials(await client.authCredentialsGet({}));
      await requireSameActor(actor, dependencies);
      if (!credentials.googleLinked) throw new ManagedGoogleIdentityUnavailableError();
      return credentials;
    }
    if (response.status !== 202 || !hasExactKeys(result, ["state"]) || result.state !== "pending") {
      throw new ManagedGoogleIdentityUnavailableError();
    }
    const retrySeconds = Number(response.headers.get("retry-after"));
    const delayMs = Number.isSafeInteger(retrySeconds) && retrySeconds >= 1 && retrySeconds <= 5
      ? retrySeconds * 1_000
      : 1_000;
    await dependencies.sleep(delayMs);
  }
  throw new ManagedGoogleIdentityUnavailableError();
}

export async function addManagedPassword(
  input: { password: string; confirmation: string },
  dependencies: ManagedCredentialsDependencies = defaultDependencies(),
): Promise<ManagedCredentials> {
  const issue = managedNewPasswordIssue(input.password, input.confirmation);
  if (issue) throw new ManagedNewPasswordError(issue);
  const actor = await currentActor(dependencies);
  const client = dependencies.clientForRoute(actor.routeKey);
  const response = await client.authPasswordAdd({ newPassword: input.password });
  if (response.ok !== true) throw new Error("managed password add failed");
  await requireSameActor(actor, dependencies);
  const credentials = exactCredentials(await client.authCredentialsGet({}));
  await requireSameActor(actor, dependencies);
  if (!credentials.hasPassword || !credentials.googleLinked) {
    throw new Error("managed password credential proof failed");
  }
  return credentials;
}
