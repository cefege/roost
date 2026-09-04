/**
 * Sends one signed canonical request to the private SaaS provisioner socket.
 * Gateway provisioning calls this boundary after validating operation-specific payloads.
 * Strict response parsing keeps privileged worker output inside the canonical IPC contract.
 */

import { canonicalJson, parseStrictJson, type CanonicalJsonValue } from "./canonical-json.ts";
import { exchangePrivateIpcFrame } from "./private-ipc-framing.ts";
import {
  PRIVATE_IPC_SOCKET_PATH,
  serializePrivateIpcEnvelope,
  signPrivateIpcEnvelope,
  type PrivateIpcRequest,
  type PrivateIpcSigningKey,
  type SignedPrivateIpcEnvelope,
} from "./private-ipc.ts";

export class PrivateIpcRemoteError extends Error {
  constructor(readonly code: string) { super("private provisioner request failed"); this.name = "PrivateIpcRemoteError"; }
}

function exactObject(value: CanonicalJsonValue): Record<string, CanonicalJsonValue> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new PrivateIpcRemoteError("invalid-response");
  return value;
}

export async function sendPrivateIpcEnvelope(envelope: SignedPrivateIpcEnvelope, options: { socketPath?: string; timeoutMs?: number } = {}): Promise<CanonicalJsonValue> {
  const payload = serializePrivateIpcEnvelope(envelope);
  const raw = await exchangePrivateIpcFrame(options.socketPath ?? PRIVATE_IPC_SOCKET_PATH, payload, { timeoutMs: options.timeoutMs });
  let parsed: CanonicalJsonValue;
  try { parsed = parseStrictJson(raw); canonicalJson(parsed); } catch { throw new PrivateIpcRemoteError("invalid-response"); }
  const result = exactObject(parsed);
  const keys = Object.keys(result).sort();
  if (result.ok === true && keys.length === 2 && keys[0] === "body" && keys[1] === "ok") return result.body!;
  if (result.ok === false && keys.length === 2 && keys[0] === "error" && keys[1] === "ok" && typeof result.error === "string") throw new PrivateIpcRemoteError(result.error);
  throw new PrivateIpcRemoteError("invalid-response");
}

export async function callPrivateProvisioner(request: PrivateIpcRequest, privateKey: PrivateIpcSigningKey, options: { socketPath?: string; timeoutMs?: number; issuedAtMs?: number; nonce?: string } = {}): Promise<CanonicalJsonValue> {
  return sendPrivateIpcEnvelope(signPrivateIpcEnvelope(request, privateKey, options), options);
}
