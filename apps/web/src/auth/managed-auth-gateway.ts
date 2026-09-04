// This module owns the same-origin HTTP contract used before a managed browser can sign RPCs.
// Managed signup, Google login, and credential-link flows call it for bounded gateway exchanges.
// It depends only on fetch and tenant-route validation so untrusted responses stay tightly checked.

import { isTenantRouteKey } from "@roost/shared/tenant-route";

const MAX_GATEWAY_RESPONSE_BYTES = 16 * 1024;
const GOOGLE_AUTHORIZATION_ORIGIN = "https://accounts.google.com";
const GOOGLE_AUTHORIZATION_PATH = "/o/oauth2/v2/auth";

type JsonObject = Record<string, unknown>;

export interface ManagedAuthConfig {
  signupEnabled: boolean;
  googleEnabled: boolean;
  turnstileSiteKey: string;
}

export type ManagedAuthResult =
  | { state: "pending"; retryAfterMs: number }
  | { state: "awaiting-device"; routeKey: string }
  | { state: "ready"; routeKey: string; assertion?: string }
  | { state: "proof-required" | "capacity" | "failed" };

export class ManagedAuthGatewayError extends Error {
  constructor(readonly status: number, readonly code: string | null = null) {
    super("managed auth gateway request failed");
    this.name = "ManagedAuthGatewayError";
  }
}

function exactObject(value: unknown): JsonObject {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new ManagedAuthGatewayError(502);
  }
  return value as JsonObject;
}

async function boundedJson(response: Response): Promise<JsonObject> {
  const text = await response.text();
  if (new TextEncoder().encode(text).byteLength > MAX_GATEWAY_RESPONSE_BYTES) {
    throw new ManagedAuthGatewayError(502);
  }
  try {
    return exactObject(JSON.parse(text));
  } catch (error) {
    if (error instanceof ManagedAuthGatewayError) throw error;
    throw new ManagedAuthGatewayError(502);
  }
}

async function gatewayRequest(
  path: string,
  init: RequestInit,
  fetchImpl: typeof fetch = globalThis.fetch,
): Promise<{ response: Response; body: JsonObject }> {
  let response: Response;
  try {
    response = await fetchImpl(path, {
      ...init,
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      referrerPolicy: "no-referrer",
      headers: {
        Accept: "application/json",
        ...(init.body === undefined ? {} : { "Content-Type": "application/json" }),
      },
    });
  } catch {
    throw new ManagedAuthGatewayError(0);
  }
  const body = await boundedJson(response);
  if (!response.ok) {
    throw new ManagedAuthGatewayError(
      response.status,
      typeof body.error === "string" && body.error.length <= 64 ? body.error : null,
    );
  }
  return { response, body };
}

export async function getManagedAuthConfig(
  fetchImpl: typeof fetch = globalThis.fetch,
): Promise<ManagedAuthConfig> {
  const { body } = await gatewayRequest(
    "/__roost/auth/config",
    { method: "GET" },
    fetchImpl,
  );
  if (
    typeof body.signupEnabled !== "boolean"
    || typeof body.googleEnabled !== "boolean"
    || typeof body.turnstileSiteKey !== "string"
    || body.turnstileSiteKey.length > 256
  ) {
    throw new ManagedAuthGatewayError(502);
  }
  return {
    signupEnabled: body.signupEnabled,
    googleEnabled: body.googleEnabled,
    turnstileSiteKey: body.turnstileSiteKey,
  };
}

export async function startManagedEmailSignup(
  input: { email: string; turnstileToken: string },
  fetchImpl: typeof fetch = globalThis.fetch,
): Promise<void> {
  const { body } = await gatewayRequest(
    "/__roost/signup/email/start",
    { method: "POST", body: JSON.stringify(input) },
    fetchImpl,
  );
  if (body.state !== "verification-pending") throw new ManagedAuthGatewayError(502);
}

export async function verifyManagedEmailSignup(
  token: string,
  fetchImpl: typeof fetch = globalThis.fetch,
): Promise<void> {
  const { body } = await gatewayRequest(
    "/__roost/signup/email/verify",
    { method: "POST", body: JSON.stringify({ token }) },
    fetchImpl,
  );
  if (body.state !== "pending") throw new ManagedAuthGatewayError(502);
}

export async function startManagedGoogle(
  input:
    | { intent: "login" }
    | { intent: "signup"; turnstileToken: string }
    | { intent: "link"; routeKey: string; linkTicket: string },
  fetchImpl: typeof fetch = globalThis.fetch,
): Promise<string> {
  const { body } = await gatewayRequest(
    "/__roost/auth/google/start",
    { method: "POST", body: JSON.stringify(input) },
    fetchImpl,
  );
  if (typeof body.authorizationUrl !== "string" || body.authorizationUrl.length > 4_096) {
    throw new ManagedAuthGatewayError(502);
  }
  let authorizationUrl: URL;
  try {
    authorizationUrl = new URL(body.authorizationUrl);
  } catch {
    throw new ManagedAuthGatewayError(502);
  }
  if (
    authorizationUrl.origin !== GOOGLE_AUTHORIZATION_ORIGIN
    || authorizationUrl.pathname !== GOOGLE_AUTHORIZATION_PATH
    || authorizationUrl.username
    || authorizationUrl.password
    || authorizationUrl.hash
  ) {
    throw new ManagedAuthGatewayError(502);
  }
  return authorizationUrl.href;
}

export async function getManagedAuthResult(
  fetchImpl: typeof fetch = globalThis.fetch,
): Promise<ManagedAuthResult> {
  const { response, body } = await gatewayRequest(
    "/__roost/auth/result",
    { method: "GET" },
    fetchImpl,
  );
  if (body.state === "pending") {
    const retryAfter = response.headers.get("retry-after");
    const seconds = retryAfter === null ? 1 : Number(retryAfter);
    return {
      state: "pending",
      retryAfterMs: Number.isSafeInteger(seconds) && seconds >= 1 && seconds <= 30
        ? seconds * 1_000
        : 1_000,
    };
  }
  if (body.state === "awaiting-device" && isTenantRouteKey(body.routeKey)) {
    return { state: "awaiting-device", routeKey: body.routeKey };
  }
  if (body.state === "ready" && isTenantRouteKey(body.routeKey)) {
    if (body.assertion === undefined) return { state: "ready", routeKey: body.routeKey };
    if (typeof body.assertion === "string" && body.assertion.length > 0 && body.assertion.length <= 12_000) {
      return { state: "ready", routeKey: body.routeKey, assertion: body.assertion };
    }
  }
  if (body.state === "proof-required" || body.state === "capacity" || body.state === "failed") {
    return { state: body.state };
  }
  throw new ManagedAuthGatewayError(502);
}

export async function bindManagedGoogleDevice(
  sshPubkeyB64: string,
  fetchImpl: typeof fetch = globalThis.fetch,
): Promise<{ routeKey: string; assertion: string }> {
  const { body } = await gatewayRequest(
    "/__roost/auth/bind-device",
    { method: "POST", body: JSON.stringify({ sshPubkeyB64 }) },
    fetchImpl,
  );
  if (
    body.state !== "ready"
    || !isTenantRouteKey(body.routeKey)
    || typeof body.assertion !== "string"
    || body.assertion.length === 0
    || body.assertion.length > 12_000
  ) {
    throw new ManagedAuthGatewayError(502);
  }
  return { routeKey: body.routeKey, assertion: body.assertion };
}
