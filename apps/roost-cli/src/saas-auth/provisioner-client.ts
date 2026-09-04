/**
 * Validates signup provisioning operations before sending them over private IPC.
 * Email, Google, and result protocols use this client instead of constructing envelopes directly.
 * Exact request and response shapes keep privileged provisioning actions fail-closed.
 */

import type { KeyLike } from "node:crypto";
import type { CanonicalJsonValue } from "./canonical-json.ts";
import { callPrivateProvisioner } from "./private-ipc-client.ts";
import type { PrivateIpcRequest, SubmitBody } from "./private-ipc.ts";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u;
const HASH_RE = /^[0-9a-f]{64}$/u;

export interface CentralAssertionInputs {
  purpose: "continue" | "link";
  accountId: string;
  coordinatorId: string;
  routeKey: string;
  identityIssuer: "https://accounts.google.com";
  identitySubject: string;
  emailNormalized: string;
  deviceFingerprint?: string;
  jti?: string;
  issuedAtMs?: number;
  expiresAtMs?: number;
}

export type ProvisionerSubmitResult =
  | { state: "pending"; jobId: string; retryAfterMs: number }
  | { state: "proof-required" | "capacity" | "failed" };

export type ProvisionerStatus =
  | { state: "pending"; retryAfterMs: number }
  | { state: "ready"; routeKey: string }
  | { state: "awaiting-device" | "ready"; routeKey: string; assertionInputs: CentralAssertionInputs }
  | { state: "failed" };

export type ProvisionerCall = (request: PrivateIpcRequest) => Promise<CanonicalJsonValue>;

function plainObject(value: unknown): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error("invalid provisioner response");
  return value as Record<string, unknown>;
}

function exactKeys(value: Record<string, unknown>, fields: readonly string[]): void {
  const actual = Object.keys(value).sort();
  const expected = [...fields].sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    throw new Error("invalid provisioner response");
  }
}

function retryDelay(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 250 || value > 60_000) {
    throw new Error("invalid provisioner response");
  }
  return value;
}

export function parseCentralAssertionInputs(value: unknown): CentralAssertionInputs {
  const object = plainObject(value);
  const required = ["purpose", "accountId", "coordinatorId", "routeKey", "identityIssuer", "identitySubject", "emailNormalized"];
  const optional = ["deviceFingerprint", "jti", "issuedAtMs", "expiresAtMs"];
  const keys = Object.keys(object);
  if (required.some((field) => !keys.includes(field)) || keys.some((field) => !required.includes(field) && !optional.includes(field))) {
    throw new Error("invalid provisioner response");
  }
  if (
    (object.purpose !== "continue" && object.purpose !== "link")
    || typeof object.accountId !== "string" || !UUID_RE.test(object.accountId)
    || typeof object.coordinatorId !== "string" || !UUID_RE.test(object.coordinatorId)
    || typeof object.routeKey !== "string" || !HASH_RE.test(object.routeKey)
    || object.identityIssuer !== "https://accounts.google.com"
    || typeof object.identitySubject !== "string" || object.identitySubject.length === 0 || Buffer.byteLength(object.identitySubject, "utf8") > 255
    || typeof object.emailNormalized !== "string" || object.emailNormalized.length === 0 || Buffer.byteLength(object.emailNormalized, "utf8") > 320
  ) throw new Error("invalid provisioner response");
  const result: CentralAssertionInputs = {
    purpose: object.purpose,
    accountId: object.accountId,
    coordinatorId: object.coordinatorId,
    routeKey: object.routeKey,
    identityIssuer: object.identityIssuer,
    identitySubject: object.identitySubject,
    emailNormalized: object.emailNormalized,
  };
  const hasBoundFields = optional.some((field) => object[field] !== undefined);
  if (hasBoundFields) {
    if (
      typeof object.deviceFingerprint !== "string" || !HASH_RE.test(object.deviceFingerprint)
      || typeof object.jti !== "string" || !UUID_RE.test(object.jti)
      || typeof object.issuedAtMs !== "number" || !Number.isSafeInteger(object.issuedAtMs) || object.issuedAtMs < 0
      || typeof object.expiresAtMs !== "number" || !Number.isSafeInteger(object.expiresAtMs) || object.expiresAtMs <= object.issuedAtMs
    ) throw new Error("invalid provisioner response");
    result.deviceFingerprint = object.deviceFingerprint;
    result.jti = object.jti;
    result.issuedAtMs = object.issuedAtMs;
    result.expiresAtMs = object.expiresAtMs;
  }
  if ((result.purpose === "link") !== hasBoundFields) throw new Error("invalid provisioner response");
  return result;
}

function unwrap(value: CanonicalJsonValue): Record<string, unknown> {
  const object = plainObject(value);
  if (object.ok !== true) throw new Error("invalid provisioner response");
  const { ok: _ok, ...body } = object;
  return body;
}

function submitResult(value: CanonicalJsonValue): ProvisionerSubmitResult {
  const object = unwrap(value);
  if (object.state === "pending") {
    exactKeys(object, ["state", "jobId", "retryAfterMs"]);
    if (typeof object.jobId !== "string" || !UUID_RE.test(object.jobId)) throw new Error("invalid provisioner response");
    return { state: "pending", jobId: object.jobId, retryAfterMs: retryDelay(object.retryAfterMs) };
  }
  exactKeys(object, ["state"]);
  if (object.state !== "proof-required" && object.state !== "capacity" && object.state !== "failed") {
    throw new Error("invalid provisioner response");
  }
  return { state: object.state };
}

function statusResult(value: CanonicalJsonValue): ProvisionerStatus {
  const object = unwrap(value);
  if (object.state === "pending") {
    exactKeys(object, ["state", "retryAfterMs"]);
    return { state: "pending", retryAfterMs: retryDelay(object.retryAfterMs) };
  }
  if (object.state === "failed") {
    exactKeys(object, ["state"]);
    return { state: "failed" };
  }
  if (object.state !== "ready" && object.state !== "awaiting-device") throw new Error("invalid provisioner response");
  if (object.assertionInputs === undefined) {
    exactKeys(object, ["state", "routeKey"]);
    if (object.state !== "ready" || typeof object.routeKey !== "string" || !HASH_RE.test(object.routeKey)) throw new Error("invalid provisioner response");
    return { state: "ready", routeKey: object.routeKey };
  }
  exactKeys(object, ["state", "routeKey", "assertionInputs"]);
  if (typeof object.routeKey !== "string" || !HASH_RE.test(object.routeKey)) throw new Error("invalid provisioner response");
  const inputs = parseCentralAssertionInputs(object.assertionInputs);
  if (inputs.routeKey !== object.routeKey) throw new Error("invalid provisioner response");
  return { state: object.state, routeKey: object.routeKey, assertionInputs: inputs };
}

export class ProvisionerClient {
  constructor(private readonly call: ProvisionerCall) {}

  submit(body: SubmitBody): Promise<ProvisionerSubmitResult> {
    return this.call({ purpose: "submit", body }).then(submitResult);
  }

  status(jobId: string): Promise<ProvisionerStatus> {
    return this.call({ purpose: "status", body: { jobId } }).then(statusResult);
  }

  finalizeLink(jobId: string): Promise<ProvisionerStatus> {
    return this.call({ purpose: "finalize-link", body: { jobId } }).then(statusResult);
  }
}

export function createPrivateProvisionerClient(privateKey: KeyLike): ProvisionerClient {
  return new ProvisionerClient((request) => callPrivateProvisioner(request, privateKey));
}
