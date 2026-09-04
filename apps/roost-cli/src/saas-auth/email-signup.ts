/**
 * Coordinates email signup challenges from abuse checks through provisioning submission.
 * The HTTP gateway calls this protocol and persists every capability through GatewayStateStore.
 * Keeping token hashes and result receipts coupled prevents replay or cross-browser disclosure.
 */

import { createHash, randomBytes } from "node:crypto";
import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import { canonicalJson } from "./canonical-json.ts";
import type { SignupEmailOutbox } from "./signup-email-outbox.ts";
import type { GatewayStateStore } from "./state-store.ts";
import type { TurnstileVerifier } from "./turnstile.ts";
import type { ProvisionerClient, ProvisionerSubmitResult } from "./provisioner-client.ts";
import { gatewayJson, secureCookie, InvalidGatewayRequest, exactObject, boundedText } from "./request-security.ts";

const HOUR_MS = 60 * 60 * 1_000;
const RESULT_COOKIE = "__Host-roost_signup";
const TOKEN_RE = /^[A-Za-z0-9_-]{43}$/u;

export interface EmailSignupProtocolOptions {
  store: GatewayStateStore;
  outbox: SignupEmailOutbox;
  turnstile: TurnstileVerifier;
  provisioner: ProvisionerClient;
  signupEnabled: boolean;
  now?: () => number;
}

export class EmailSignupProtocol {
  private readonly now: () => number;
  private dispatching = false;

  constructor(private readonly options: EmailSignupProtocolOptions) {
    this.now = options.now ?? Date.now;
  }

  private dispatchOutbox(): void {
    if (this.dispatching) return;
    this.dispatching = true;
    queueMicrotask(() => {
      void this.options.outbox.runOnce().catch(() => undefined).finally(() => {
        this.dispatching = false;
      });
    });
  }

  async start(body: unknown, clientIp: string): Promise<Response> {
    if (!this.options.signupEnabled) return gatewayJson({ error: "signup-unavailable" }, 503);
    const input = exactObject(body, ["email", "turnstileToken"]);
    const emailRaw = boundedText(input.email, 320);
    const emailNormalized = normalizeAccountEmail(emailRaw);
    if (!emailNormalized) throw new InvalidGatewayRequest();
    const nowMs = this.now();
    const ipRate = this.options.store.consumeRateBucket({
      scope: "email-start-ip", key: clientIp, limit: 5, windowMs: HOUR_MS, nowMs,
    });
    const emailRate = this.options.store.consumeRateBucket({
      scope: "email-start-email", key: emailNormalized, limit: 3, windowMs: HOUR_MS, nowMs,
    });
    const pending = gatewayJson({ state: "verification-pending" }, 202);
    if (!ipRate.allowed || !emailRate.allowed) return pending;
    if (!await this.options.turnstile.verify(input.turnstileToken, clientIp)) {
      return gatewayJson({ error: "verification-failed" }, 400);
    }
    this.options.outbox.queueVerification({
      emailNormalized,
      nowMs,
      payload: (token) => ({
        subject: "Finish setting up your Roost account",
        html: `<p>Finish setting up your Roost account:</p><p><a href="https://dashboard.roosttt.com/signup/verify#${token}">Verify email</a></p>`,
        text: `Finish setting up your Roost account: https://dashboard.roosttt.com/signup/verify#${token}`,
      }),
    });
    this.dispatchOutbox();
    return pending;
  }

  async verify(body: unknown): Promise<Response> {
    if (!this.options.signupEnabled) return gatewayJson({ error: "signup-unavailable" }, 503);
    const input = exactObject(body, ["token"]);
    const token = boundedText(input.token, 43);
    if (!TOKEN_RE.test(token) || Buffer.from(token, "base64url").byteLength !== 32) throw new InvalidGatewayRequest();
    const activationTokenHash = createHash("sha256").update(token, "utf8").digest("hex");
    const challenge = this.options.store.verifyEmailChallenge(activationTokenHash, this.now());
    if (!challenge || challenge.verifiedAtMs === null) return gatewayJson({ error: "verification-failed" }, 400);
    const semantic = {
      challengeId: challenge.id,
      emailNormalized: challenge.emailNormalized,
      activationTokenHash,
      verifiedAtMs: challenge.verifiedAtMs,
      expiresAtMs: challenge.expiresAtMs,
    };
    const idempotencyKey = createHash("sha256")
      .update("roost-verified-email:v1\0", "utf8")
      .update(canonicalJson(semantic), "utf8")
      .digest("hex");
    let submitted: ProvisionerSubmitResult;
    try {
      submitted = await this.options.provisioner.submit({
        kind: "verified-email",
        submission: { ...semantic, idempotencyKey },
      });
    } catch {
      return gatewayJson({ error: "request unavailable" }, 503);
    }
    const receipt = randomBytes(32).toString("base64url");
    const jobId = submitted.state === "pending" ? submitted.jobId : challenge.id;
    this.options.store.createResultReceipt({
      receipt,
      browserCookie: receipt,
      jobId,
      nowMs: this.now(),
      expiresAtMs: challenge.expiresAtMs,
    });
    if (submitted.state !== "pending") {
      this.options.store.setResultOutcome({ jobId, state: submitted.state, nowMs: this.now() });
    }
    this.options.store.consumeEmailChallenge(challenge.id, this.now());
    return gatewayJson({ state: "pending" }, 202, {
      "set-cookie": secureCookie(RESULT_COOKIE, receipt, Math.max(1, Math.floor((challenge.expiresAtMs - this.now()) / 1_000))),
    });
  }
}
