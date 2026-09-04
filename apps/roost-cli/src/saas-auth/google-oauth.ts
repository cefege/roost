/**
 * Owns Google OAuth login, signup, link, and callback transitions for the gateway.
 * HTTP routes call this protocol with durable state, Turnstile, and provisioning dependencies.
 * PKCE, nonce, signed cookies, and one-time attempts bind callbacks to the initiating browser.
 */

import { createHash, randomBytes } from "node:crypto";
import type { JWTVerifyGetKey } from "jose";
import { canonicalJson } from "./canonical-json.ts";
import { GATEWAY_OUTBOUND_ENDPOINTS, GATEWAY_PUBLIC_ORIGIN } from "./gateway-config.ts";
import { verifyGoogleIdToken } from "./google-id-token.ts";
import { fetchBoundedJson, type GatewayFetch } from "./provider-http.ts";
import type { ProvisionerClient, ProvisionerSubmitResult } from "./provisioner-client.ts";
import {
  boundedText,
  exactObject,
  expireCookie,
  gatewayJson,
  gatewayRedirect,
  InvalidGatewayRequest,
  requestCookie,
  secureCookie,
} from "./request-security.ts";
import { OAUTH_ATTEMPT_TTL_MS, type ConsumedOAuthAttempt, type GatewayStateStore, type OAuthIntent } from "./state-store.ts";
import type { TurnstileVerifier } from "./turnstile.ts";

export const GOOGLE_CALLBACK_URL = `${GATEWAY_PUBLIC_ORIGIN}/auth/google/callback`;
export const OAUTH_COOKIE = "__Host-roost_oauth";
export const RESULT_COOKIE = "__Host-roost_signup";
const TEN_MINUTES_MS = 10 * 60 * 1_000;
const HOUR_MS = 60 * 60 * 1_000;
const HASH_RE = /^[0-9a-f]{64}$/u;
const COMPACT_JWT_RE = /^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/u;

export interface GoogleOAuthProtocolOptions {
  store: GatewayStateStore;
  turnstile: TurnstileVerifier;
  provisioner: ProvisionerClient;
  googleEnabled: boolean;
  signupEnabled: boolean;
  clientId: string;
  clientSecret: string;
  fetch?: GatewayFetch;
  now?: () => number;
  jwks?: JWTVerifyGetKey;
  /** Deterministic local token fixture seam. Production construction must omit it. */
  tokenEndpoint?: string;
}

function randomCapability(): string {
  return randomBytes(32).toString("base64url");
}

function authorizationUrl(clientId: string, state: string, nonce: string, verifier: string): string {
  const challenge = createHash("sha256").update(verifier, "ascii").digest("base64url");
  const url = new URL(GATEWAY_OUTBOUND_ENDPOINTS.googleAuthorization);
  url.searchParams.set("client_id", clientId);
  url.searchParams.set("redirect_uri", GOOGLE_CALLBACK_URL);
  url.searchParams.set("response_type", "code");
  url.searchParams.set("scope", "openid email");
  url.searchParams.set("state", state);
  url.searchParams.set("nonce", nonce);
  url.searchParams.set("code_challenge", challenge);
  url.searchParams.set("code_challenge_method", "S256");
  return url.toString();
}

function callbackFields(url: URL): { state: string; code?: string; error?: string } | null {
  const allowed: Record<string, true> = {
    state: true,
    code: true,
    error: true,
    error_description: true,
    error_uri: true,
    error_subtype: true,
    iss: true,
    scope: true,
    authuser: true,
    prompt: true,
  };
  for (const key of url.searchParams.keys()) {
    if (allowed[key] !== true || url.searchParams.getAll(key).length !== 1) return null;
  }
  const state = url.searchParams.get("state");
  if (!state || !/^[A-Za-z0-9_-]{43}$/u.test(state)) return null;
  const issuer = url.searchParams.get("iss");
  if (issuer !== null && issuer !== "https://accounts.google.com" && issuer !== "accounts.google.com") return null;
  const code = url.searchParams.get("code");
  const error = url.searchParams.get("error");
  if ((code === null) === (error === null)) return null;
  if (code !== null && (code.length === 0 || Buffer.byteLength(code, "utf8") > 4_096 || /[\u0000-\u001f\u007f]/u.test(code))) return null;
  if (error !== null && (error.length === 0 || Buffer.byteLength(error, "utf8") > 256 || /[^A-Za-z0-9._-]/u.test(error))) return null;
  const scope = url.searchParams.get("scope");
  if (scope !== null && scope.split(" ").sort().join(" ") !== "email openid") return null;
  return code === null ? { state, error: error! } : { state, code };
}

function resultResponse(receipt: string, expiresAtMs: number, nowMs: number): Response {
  return gatewayRedirect("/auth/google/complete", [
    expireCookie(OAUTH_COOKIE),
    secureCookie(RESULT_COOKIE, receipt, Math.max(1, Math.floor((expiresAtMs - nowMs) / 1_000))),
  ]);
}

export class GoogleOAuthProtocol {
  private readonly fetchImpl: GatewayFetch;
  private readonly now: () => number;
  private readonly tokenEndpoint: string;

  constructor(private readonly options: GoogleOAuthProtocolOptions) {
    this.fetchImpl = options.fetch ?? globalThis.fetch;
    this.now = options.now ?? Date.now;
    this.tokenEndpoint = options.tokenEndpoint ?? GATEWAY_OUTBOUND_ENDPOINTS.googleToken;
  }

  async start(request: Request, body: unknown, clientIp: string): Promise<Response> {
    if (!this.options.googleEnabled) return gatewayJson({ error: "request unavailable" }, 503);
    if (body === null || typeof body !== "object" || Array.isArray(body)) throw new InvalidGatewayRequest();
    const intent = (body as Record<string, unknown>).intent;
    if (intent !== "login" && intent !== "signup" && intent !== "link") throw new InvalidGatewayRequest();
    let routeKey: string | undefined;
    let linkTicket: string | undefined;
    let proofAtMs: number | undefined;
    if (intent === "login") {
      exactObject(body, ["intent"]);
      const rate = this.options.store.consumeRateBucket({
        scope: "google-login-ip", key: clientIp, limit: 30, windowMs: TEN_MINUTES_MS, nowMs: this.now(),
      });
      if (!rate.allowed) return gatewayJson({ error: "request rejected" }, 429, { "retry-after": String(Math.max(1, Math.ceil((rate.retryAtMs - this.now()) / 1_000))) });
    } else if (intent === "signup") {
      if (!this.options.signupEnabled) return gatewayJson({ error: "signup-unavailable" }, 503);
      const input = exactObject(body, ["intent", "turnstileToken"]);
      const rate = this.options.store.consumeRateBucket({
        scope: "google-signup-ip", key: clientIp, limit: 5, windowMs: HOUR_MS, nowMs: this.now(),
      });
      if (!rate.allowed) return gatewayJson({ error: "request rejected" }, 429, { "retry-after": String(Math.max(1, Math.ceil((rate.retryAtMs - this.now()) / 1_000))) });
      if (!await this.options.turnstile.verify(input.turnstileToken, clientIp)) return gatewayJson({ error: "verification-failed" }, 400);
      proofAtMs = this.now();
    } else {
      const input = exactObject(body, ["intent", "routeKey", "linkTicket"]);
      routeKey = boundedText(input.routeKey, 64);
      linkTicket = boundedText(input.linkTicket, 8_192);
      if (!HASH_RE.test(routeKey) || !COMPACT_JWT_RE.test(linkTicket)) throw new InvalidGatewayRequest();
    }
    const state = randomCapability();
    const nonce = randomCapability();
    const verifier = randomCapability();
    const oauthCookie = randomCapability();
    const browserCookie = requestCookie(request, RESULT_COOKIE) ?? randomCapability();
    this.options.store.startOAuthAttempt({
      browserCookie,
      oauthCookie,
      state,
      pkceVerifier: verifier,
      nonce,
      intent,
      ...(routeKey === undefined ? {} : { routeKey, linkTicket }),
      ...(proofAtMs === undefined ? {} : { proofAtMs }),
      nowMs: this.now(),
    });
    const response = gatewayJson({ authorizationUrl: authorizationUrl(this.options.clientId, state, nonce, verifier) }, 200);
    response.headers.append("set-cookie", secureCookie(OAUTH_COOKIE, oauthCookie, Math.floor(OAUTH_ATTEMPT_TTL_MS / 1_000)));
    response.headers.append("set-cookie", secureCookie(RESULT_COOKIE, browserCookie, Math.floor(OAUTH_ATTEMPT_TTL_MS / 1_000)));
    return response;
  }

  private async exchange(code: string, attempt: ConsumedOAuthAttempt): Promise<string> {
    const body = new URLSearchParams({
      code,
      client_id: this.options.clientId,
      client_secret: this.options.clientSecret,
      redirect_uri: GOOGLE_CALLBACK_URL,
      grant_type: "authorization_code",
      code_verifier: attempt.pkceVerifier,
    });
    const { response, value } = await fetchBoundedJson(this.fetchImpl, this.tokenEndpoint, {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body,
    }, { timeoutMs: 5_000, maxResponseBytes: 64 * 1024 });
    if (!response.ok || value === null || typeof value !== "object" || Array.isArray(value)) throw new Error("token exchange failed");
    const idToken = (value as Record<string, unknown>).id_token;
    if (typeof idToken !== "string" || Buffer.byteLength(idToken, "utf8") > 16 * 1024 || !COMPACT_JWT_RE.test(idToken)) {
      throw new Error("token exchange failed");
    }
    return idToken;
  }

  private createResult(attempt: ConsumedOAuthAttempt, submitted: ProvisionerSubmitResult): { receipt: string; expiresAtMs: number } {
    const nowMs = this.now();
    const expiresAtMs = nowMs + TEN_MINUTES_MS;
    const receipt = randomCapability();
    const jobId = submitted.state === "pending" ? submitted.jobId : attempt.id;
    this.options.store.createResultReceiptForBrowserHash({
      receipt,
      browserCookieHash: attempt.browserCookieHash,
      jobId,
      nowMs,
      expiresAtMs,
    });
    if (submitted.state !== "pending") this.options.store.setResultOutcome({ jobId, state: submitted.state, nowMs });
    return { receipt, expiresAtMs };
  }

  private failedResult(attempt: ConsumedOAuthAttempt): { receipt: string; expiresAtMs: number } {
    return this.createResult(attempt, { state: "failed" });
  }

  async callback(request: Request, url: URL): Promise<Response> {
    if (!this.options.googleEnabled) {
      return gatewayRedirect("/auth/google/complete", [expireCookie(OAUTH_COOKIE)]);
    }
    const fields = callbackFields(url);
    const oauthCookie = requestCookie(request, OAUTH_COOKIE);
    if (!fields || !oauthCookie) return gatewayRedirect("/auth/google/complete", [expireCookie(OAUTH_COOKIE)]);
    const attempt = this.options.store.consumeOAuthAttempt(oauthCookie, fields.state, this.now());
    if (!attempt) return gatewayRedirect("/auth/google/complete", [expireCookie(OAUTH_COOKIE)]);
    if (attempt.intent === "signup" && !this.options.signupEnabled) {
      return gatewayRedirect("/auth/google/complete", [expireCookie(OAUTH_COOKIE)]);
    }
    if (fields.error !== undefined) {
      const failed = this.failedResult(attempt);
      return resultResponse(failed.receipt, failed.expiresAtMs, this.now());
    }
    try {
      const idToken = await this.exchange(fields.code!, attempt);
      const claims = await verifyGoogleIdToken(idToken, {
        clientId: this.options.clientId,
        expectedNonce: attempt.nonce,
        ...(this.options.jwks === undefined ? {} : { jwks: this.options.jwks }),
      });
      const idempotencyKey = createHash("sha256")
        .update("roost-google-submission:v1\0", "utf8")
        .update(canonicalJson({ attemptId: attempt.id, intent: attempt.intent, issuer: claims.issuer, subject: claims.subject }), "utf8")
        .digest("hex");
      const submission = {
        emailNormalized: claims.emailNormalized,
        identityIssuer: claims.issuer,
        identitySubject: claims.subject,
        verifiedAtMs: this.now(),
        idempotencyKey,
      };
      const body = attempt.intent === "link"
        ? { kind: "google-link" as const, submission: { ...submission, routeKey: attempt.routeKey!, linkTicket: attempt.linkTicket! } }
        : { kind: `google-${attempt.intent}` as "google-login" | "google-signup", submission };
      const submitted = await this.options.provisioner.submit(body);
      const result = this.createResult(attempt, submitted);
      return resultResponse(result.receipt, result.expiresAtMs, this.now());
    } catch {
      const failed = this.failedResult(attempt);
      return resultResponse(failed.receipt, failed.expiresAtMs, this.now());
    }
  }
}
