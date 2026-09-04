/**
 * Defines and authenticates canonical envelopes crossing the private provisioner socket.
 * Gateway and root-side server share these request shapes, signatures, and freshness checks.
 * Domain-separated bytes and strict parsing prevent replay, substitution, and parser ambiguity.
 */

import {
  createPublicKey,
  randomBytes,
  sign as ed25519Sign,
  verify as ed25519Verify,
  type KeyLike,
  type KeyObject,
} from "node:crypto";
import { Buffer } from "node:buffer";
import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import { canonicalJson, parseStrictJson, type CanonicalJsonValue } from "./canonical-json.ts";

export const PRIVATE_IPC_SOCKET_PATH = "/run/roost-saas-private/provision.sock";
export const PRIVATE_IPC_MAX_FRAME_BYTES = 16 * 1024;
export const PRIVATE_IPC_TIMEOUT_MS = 5_000;
export const PRIVATE_IPC_MAX_CLOCK_SKEW_MS = 30_000;
export const PRIVATE_IPC_SIGNATURE_PREFIX = "roost-saas-ipc:v1\0";
export const PRIVATE_IPC_PURPOSES = ["submit", "status", "finalize-link"] as const;
export type PrivateIpcPurpose = typeof PRIVATE_IPC_PURPOSES[number];

export interface VerifiedEmailSubmissionDto {
  challengeId: string;
  emailNormalized: string;
  activationTokenHash: string;
  verifiedAtMs: number;
  expiresAtMs: number;
  idempotencyKey: string;
}
export interface GoogleSubmissionDto {
  emailNormalized: string;
  identityIssuer: "https://accounts.google.com";
  identitySubject: string;
  verifiedAtMs: number;
  idempotencyKey: string;
}
export interface GoogleLinkSubmissionDto extends GoogleSubmissionDto {
  routeKey: string;
  linkTicket: string;
}
export type SubmitBody =
  | { kind: "verified-email"; submission: VerifiedEmailSubmissionDto }
  | { kind: "google-signup" | "google-login"; submission: GoogleSubmissionDto }
  | { kind: "google-link"; submission: GoogleLinkSubmissionDto };
export interface StatusBody { jobId: string }
export interface FinalizeLinkBody { jobId: string }
export type PrivateIpcRequest =
  | { purpose: "submit"; body: SubmitBody }
  | { purpose: "status"; body: StatusBody }
  | { purpose: "finalize-link"; body: FinalizeLinkBody };

export interface UnsignedPrivateIpcEnvelope {
  v: 1;
  purpose: PrivateIpcPurpose;
  issuedAtMs: number;
  nonce: string;
  body: SubmitBody | StatusBody | FinalizeLinkBody;
}
export interface SignedPrivateIpcEnvelope extends UnsignedPrivateIpcEnvelope { signature: string }
export type PrivateIpcSigningKey = KeyLike | CryptoKey;
export type PrivateIpcVerificationKey = KeyLike | CryptoKey;

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const SHA256_RE = /^[0-9a-f]{64}$/;
const BASE64URL_RE = /^[A-Za-z0-9_-]+$/;
const COMPACT_JWT_RE = /^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/;

export class PrivateIpcProtocolError extends Error {
  constructor(readonly code: "invalid-request" | "unauthorized" | "stale-request", message: string) {
    super(message);
    this.name = "PrivateIpcProtocolError";
  }
}

function object(value: unknown, label: string): Record<string, CanonicalJsonValue> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new PrivateIpcProtocolError("invalid-request", `${label} must be an object`);
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) throw new PrivateIpcProtocolError("invalid-request", `${label} must be a plain object`);
  return value as Record<string, CanonicalJsonValue>;
}
function exact(value: Record<string, CanonicalJsonValue>, names: readonly string[], label: string): void {
  const keys = Object.keys(value).sort(); const expected = [...names].sort();
  if (keys.length !== expected.length || keys.some((key, index) => key !== expected[index])) throw new PrivateIpcProtocolError("invalid-request", `${label} has unknown or missing fields`);
}
function text(value: CanonicalJsonValue, label: string, maxBytes: number): string {
  if (typeof value !== "string" || value.length === 0 || Buffer.byteLength(value, "utf8") > maxBytes || /[\u0000-\u001f\u007f]/.test(value)) throw new PrivateIpcProtocolError("invalid-request", `${label} is invalid`);
  return value;
}
function timestamp(value: CanonicalJsonValue, label: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) throw new PrivateIpcProtocolError("invalid-request", `${label} is invalid`);
  return value;
}
function normalizedEmail(value: CanonicalJsonValue): string {
  const email = text(value, "emailNormalized", 320);
  if (normalizeAccountEmail(email) !== email) throw new PrivateIpcProtocolError("invalid-request", "emailNormalized is not canonical");
  return email;
}
function nonce(value: CanonicalJsonValue): string {
  const encoded = text(value, "nonce", 43);
  if (!BASE64URL_RE.test(encoded)) throw new PrivateIpcProtocolError("invalid-request", "nonce is invalid");
  const decoded = Buffer.from(encoded, "base64url");
  if (decoded.length !== 32 || decoded.toString("base64url") !== encoded) throw new PrivateIpcProtocolError("invalid-request", "nonce is invalid");
  return encoded;
}
function signature(value: CanonicalJsonValue): string {
  const encoded = text(value, "signature", 86);
  if (!BASE64URL_RE.test(encoded)) throw new PrivateIpcProtocolError("invalid-request", "signature is invalid");
  const decoded = Buffer.from(encoded, "base64url");
  if (decoded.length !== 64 || decoded.toString("base64url") !== encoded) throw new PrivateIpcProtocolError("invalid-request", "signature is invalid");
  return encoded;
}
function id(value: CanonicalJsonValue, label: string): string {
  const result = text(value, label, 36);
  if (!UUID_RE.test(result)) throw new PrivateIpcProtocolError("invalid-request", `${label} is invalid`);
  return result;
}
function hash(value: CanonicalJsonValue, label: string): string {
  const result = text(value, label, 64);
  if (!SHA256_RE.test(result)) throw new PrivateIpcProtocolError("invalid-request", `${label} is invalid`);
  return result;
}
function google(value: unknown, link: boolean): GoogleSubmissionDto | GoogleLinkSubmissionDto {
  const input = object(value, "Google submission");
  const fields = ["emailNormalized", "identityIssuer", "identitySubject", "verifiedAtMs", "idempotencyKey", ...(link ? ["routeKey", "linkTicket"] : [])];
  exact(input, fields, "Google submission");
  const identityIssuer = text(input.identityIssuer!, "identityIssuer", 28);
  if (identityIssuer !== "https://accounts.google.com") throw new PrivateIpcProtocolError("invalid-request", "identityIssuer is not canonical");
  const identitySubject = text(input.identitySubject!, "identitySubject", 255);
  if (identitySubject.trim() !== identitySubject) throw new PrivateIpcProtocolError("invalid-request", "identitySubject is invalid");
  const base: GoogleSubmissionDto = { emailNormalized: normalizedEmail(input.emailNormalized!), identityIssuer, identitySubject, verifiedAtMs: timestamp(input.verifiedAtMs!, "verifiedAtMs"), idempotencyKey: hash(input.idempotencyKey!, "idempotencyKey") };
  if (!link) return base;
  const routeKey = hash(input.routeKey!, "routeKey");
  const linkTicket = text(input.linkTicket!, "linkTicket", 8_192);
  if (!COMPACT_JWT_RE.test(linkTicket)) throw new PrivateIpcProtocolError("invalid-request", "linkTicket is invalid");
  return { ...base, routeKey, linkTicket };
}

export function validatePrivateIpcBody(purpose: PrivateIpcPurpose, value: unknown): SubmitBody | StatusBody | FinalizeLinkBody {
  const input = object(value, "body");
  if (purpose === "status" || purpose === "finalize-link") {
    exact(input, ["jobId"], "body");
    return { jobId: id(input.jobId!, "jobId") };
  }
  exact(input, ["kind", "submission"], "submit body");
  const kind = text(input.kind!, "kind", 32);
  if (kind === "verified-email") {
    const submission = object(input.submission, "verified-email submission");
    exact(submission, ["challengeId", "emailNormalized", "activationTokenHash", "verifiedAtMs", "expiresAtMs", "idempotencyKey"], "verified-email submission");
    const result: SubmitBody = { kind, submission: { challengeId: id(submission.challengeId!, "challengeId"), emailNormalized: normalizedEmail(submission.emailNormalized!), activationTokenHash: hash(submission.activationTokenHash!, "activationTokenHash"), verifiedAtMs: timestamp(submission.verifiedAtMs!, "verifiedAtMs"), expiresAtMs: timestamp(submission.expiresAtMs!, "expiresAtMs"), idempotencyKey: hash(submission.idempotencyKey!, "idempotencyKey") } };
    if (result.submission.expiresAtMs <= result.submission.verifiedAtMs) throw new PrivateIpcProtocolError("invalid-request", "verified-email expiry is invalid");
    return result;
  }
  if (kind === "google-signup" || kind === "google-login") return { kind, submission: google(input.submission, false) as GoogleSubmissionDto };
  if (kind === "google-link") return { kind, submission: google(input.submission, true) as GoogleLinkSubmissionDto };
  throw new PrivateIpcProtocolError("invalid-request", "submit kind is invalid");
}

export function parsePrivateIpcEnvelope(input: string | Uint8Array): SignedPrivateIpcEnvelope {
  let parsed: CanonicalJsonValue;
  try { parsed = parseStrictJson(input); } catch (error) { throw new PrivateIpcProtocolError("invalid-request", `envelope JSON is invalid: ${String(error)}`); }
  const envelope = object(parsed, "envelope"); exact(envelope, ["v", "purpose", "issuedAtMs", "nonce", "body", "signature"], "envelope");
  if (envelope.v !== 1) throw new PrivateIpcProtocolError("invalid-request", "envelope version is invalid");
  const purpose = text(envelope.purpose!, "purpose", 13) as PrivateIpcPurpose;
  if (!(PRIVATE_IPC_PURPOSES as readonly string[]).includes(purpose)) throw new PrivateIpcProtocolError("invalid-request", "purpose is invalid");
  return { v: 1, purpose, issuedAtMs: timestamp(envelope.issuedAtMs!, "issuedAtMs"), nonce: nonce(envelope.nonce!), body: validatePrivateIpcBody(purpose, envelope.body), signature: signature(envelope.signature!) };
}

export function privateIpcSignatureInput(envelope: UnsignedPrivateIpcEnvelope): Buffer {
  return Buffer.from(PRIVATE_IPC_SIGNATURE_PREFIX + canonicalJson(envelope), "utf8");
}

export function signPrivateIpcEnvelope(request: PrivateIpcRequest, privateKey: PrivateIpcSigningKey, options: { issuedAtMs?: number; nonce?: string } = {}): SignedPrivateIpcEnvelope {
  const unsigned: UnsignedPrivateIpcEnvelope = { v: 1, purpose: request.purpose, issuedAtMs: options.issuedAtMs ?? Date.now(), nonce: options.nonce ?? randomBytes(32).toString("base64url"), body: validatePrivateIpcBody(request.purpose, request.body) };
  timestamp(unsigned.issuedAtMs, "issuedAtMs"); nonce(unsigned.nonce);
  const signed = ed25519Sign(null, privateIpcSignatureInput(unsigned), privateKey as KeyLike).toString("base64url");
  return { ...unsigned, signature: signed };
}

export function serializePrivateIpcEnvelope(envelope: SignedPrivateIpcEnvelope): Buffer {
  return Buffer.from(canonicalJson(envelope), "utf8");
}

export function openSshEd25519PublicKey(input: string | Buffer): KeyObject {
  const line = Buffer.isBuffer(input) ? input.toString("utf8") : input;
  const match = /^ssh-ed25519 ([A-Za-z0-9+/]+={0,2})(?: [^\r\n]{1,256})?\n?$/.exec(line);
  if (!match) throw new PrivateIpcProtocolError("invalid-request", "verification key is not an OpenSSH Ed25519 public key");
  const wire = Buffer.from(match[1]!, "base64"); let offset = 0;
  const field = (): Buffer => { if (offset + 4 > wire.length) throw new PrivateIpcProtocolError("invalid-request", "verification key is malformed"); const size = wire.readUInt32BE(offset); offset += 4; if (size > wire.length - offset) throw new PrivateIpcProtocolError("invalid-request", "verification key is malformed"); const value = wire.subarray(offset, offset + size); offset += size; return value; };
  const algorithm = field(), raw = field();
  if (offset !== wire.length || algorithm.toString("ascii") !== "ssh-ed25519" || raw.length !== 32) throw new PrivateIpcProtocolError("invalid-request", "verification key is malformed");
  return createPublicKey({ key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), raw]), format: "der", type: "spki" });
}

export function verifyPrivateIpcEnvelope(envelope: SignedPrivateIpcEnvelope, publicKey: PrivateIpcVerificationKey, nowMs = Date.now()): PrivateIpcRequest {
  if (!Number.isSafeInteger(nowMs) || nowMs < 0 || Math.abs(nowMs - envelope.issuedAtMs) > PRIVATE_IPC_MAX_CLOCK_SKEW_MS) throw new PrivateIpcProtocolError("stale-request", "envelope is outside the allowed clock skew");
  const { signature: encodedSignature, ...unsigned } = envelope;
  if (!ed25519Verify(null, privateIpcSignatureInput(unsigned), publicKey as KeyLike, Buffer.from(encodedSignature, "base64url"))) throw new PrivateIpcProtocolError("unauthorized", "envelope signature is invalid");
  return { purpose: envelope.purpose, body: envelope.body } as PrivateIpcRequest;
}

export function assertPrivateIpcProofFresh(request: PrivateIpcRequest, nowMs = Date.now()): void {
  if (request.purpose !== "submit") return;
  const proof = request.body.submission;
  if (proof.verifiedAtMs > nowMs + PRIVATE_IPC_MAX_CLOCK_SKEW_MS) throw new PrivateIpcProtocolError("stale-request", "proof is from the future");
  if (request.body.kind === "verified-email") { if (nowMs >= request.body.submission.expiresAtMs) throw new PrivateIpcProtocolError("stale-request", "verified-email proof expired"); return; }
  if (nowMs - proof.verifiedAtMs > 10 * 60 * 1_000) throw new PrivateIpcProtocolError("stale-request", "Google proof expired");
}
