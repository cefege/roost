/**
 * Parses the hidden managed-instance command line and bounded seed payloads.
 * The compiled CLI dispatches here before opening the coordinator database.
 * Strict canonical forms keep privileged bootstrap inputs unambiguous and non-loggable.
 */

import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import type {
  SaasInstanceCommand,
  SeedGoogleOwnerPayload,
  SeedOwnerActivationInput,
  SeedOwnerIdentityInput,
} from "./saas-instance-types.ts";

const INSTANCE_UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const SHA256_HEX_RE = /^[0-9a-f]{64}$/;
const GOOGLE_SUBJECT_MAX_BYTES = 255;
const SEED_STDIN_MAX_BYTES = 1_024;
type SeedFlag = "--account-id" | "--coordinator-id" | "--email";

const EXPECTED_ID_FLAGS: Record<Exclude<SeedFlag, "--email">, true> = {
  "--account-id": true,
  "--coordinator-id": true,
};
const EXPECTED_ACTIVATION_FLAGS: Record<SeedFlag, true> = {
  ...EXPECTED_ID_FLAGS,
  "--email": true,
};

export function saasInstanceUsageError(message: string): Error {
  return new Error(
    `${message}. Usage: roost __saas-instance `
      + "seed-owner-activation --account-id <uuid> --coordinator-id <uuid> --email <address> | "
      + "seed-signup-gateway-owner-activation --account-id <uuid> --coordinator-id <uuid> --email <address> | "
      + "seed-google-owner --account-id <uuid> --coordinator-id <uuid> | "
      + "release-owner-activation-email | activation-status | health",
  );
}

export function normalizedInstanceUuid(raw: string | undefined): string | null {
  if (!raw || raw !== raw.trim() || !INSTANCE_UUID_RE.test(raw)) return null;
  return raw.toLowerCase();
}

export function canonicalSeedUuid(
  raw: string | undefined,
  flag: "--account-id" | "--coordinator-id",
): string {
  const normalized = normalizedInstanceUuid(raw);
  if (!normalized) throw saasInstanceUsageError(`${flag} must be a UUID`);
  return normalized;
}

function parseSeedFlags(
  args: readonly string[],
  includeEmail: boolean,
): SeedOwnerIdentityInput | SeedOwnerActivationInput {
  const expected: Readonly<Record<string, true>> = includeEmail
    ? EXPECTED_ACTIVATION_FLAGS
    : EXPECTED_ID_FLAGS;
  const values: Partial<Record<SeedFlag, string>> = {};
  for (let index = 1; index < args.length; index += 1) {
    const flag = args[index];
    if (!flag || !(flag in expected)) throw saasInstanceUsageError("unknown or positional seed argument");
    const namedFlag = flag as SeedFlag;
    if (values[namedFlag] !== undefined) throw saasInstanceUsageError(`duplicate ${flag}`);
    const value = args[index + 1];
    if (!value || value.startsWith("--")) throw saasInstanceUsageError(`missing value for ${flag}`);
    values[namedFlag] = value;
    index += 1;
  }
  const accountId = canonicalSeedUuid(values["--account-id"], "--account-id");
  const coordinatorId = canonicalSeedUuid(values["--coordinator-id"], "--coordinator-id");
  if (!includeEmail) return { accountId, coordinatorId };
  const email = normalizeAccountEmail(values["--email"] ?? "");
  if (!email) throw saasInstanceUsageError("--email must be a valid account email");
  return { accountId, coordinatorId, email };
}

/** Parse only hidden instance actions. Sensitive seed fields are deliberately
 * absent: they are accepted solely through the bounded stdin payload. */
export function parseSaasInstanceCommand(args: readonly string[]): SaasInstanceCommand {
  const action = args[0];
  if (
    action === "release-owner-activation-email"
    || action === "activation-status"
    || action === "health"
  ) {
    if (args.length !== 1) throw saasInstanceUsageError("internal action does not accept arguments");
    return { action };
  }
  if (action === "seed-google-owner") {
    return { action, input: parseSeedFlags(args, false) };
  }
  if (
    action === "seed-owner-activation"
    || action === "seed-signup-gateway-owner-activation"
  ) {
    return { action, input: parseSeedFlags(args, true) as SeedOwnerActivationInput };
  }
  throw saasInstanceUsageError("unknown internal action");
}

function decodeBoundedSeedPayload(bytes: Uint8Array): string {
  if (bytes.byteLength === 0 || bytes.byteLength > SEED_STDIN_MAX_BYTES) {
    throw new Error("internal seed stdin payload is missing or oversized");
  }
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new Error("internal seed stdin payload is not valid UTF-8");
  }
}

function oneCanonicalSeedLine(bytes: Uint8Array): string {
  const decoded = decodeBoundedSeedPayload(bytes);
  const line = decoded.endsWith("\n") ? decoded.slice(0, -1) : decoded;
  if (line.length === 0 || line.includes("\n") || line.includes("\r")) {
    throw new Error("internal seed stdin payload is not one canonical line");
  }
  return line;
}

export function parseSignupGatewayActivationHash(bytes: Uint8Array): string {
  const hash = oneCanonicalSeedLine(bytes);
  if (!SHA256_HEX_RE.test(hash)) {
    throw new Error("signup-gateway activation hash must be 64 lowercase hexadecimal characters");
  }
  return hash;
}

export function parseGoogleOwnerSeedPayload(bytes: Uint8Array): SeedGoogleOwnerPayload {
  const line = oneCanonicalSeedLine(bytes);
  let parsed: unknown;
  try {
    parsed = JSON.parse(line);
  } catch {
    throw new Error("Google owner seed stdin payload is invalid");
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("Google owner seed stdin payload is invalid");
  }
  const row = parsed as Record<string, unknown>;
  if (
    Object.keys(row).length !== 2
    || typeof row.subject !== "string"
    || typeof row.emailNormalized !== "string"
  ) {
    throw new Error("Google owner seed stdin payload is invalid");
  }
  const subject = row.subject;
  const emailNormalized = row.emailNormalized;
  if (
    subject.length === 0
    || Buffer.byteLength(subject, "utf8") > GOOGLE_SUBJECT_MAX_BYTES
    || /[\u0000-\u001f\u007f-\u009f]/u.test(subject)
  ) {
    throw new Error("Google owner seed subject is invalid");
  }
  const normalizedEmail = normalizeAccountEmail(emailNormalized);
  if (!normalizedEmail || normalizedEmail !== emailNormalized) {
    throw new Error("Google owner seed email is not normalized");
  }
  if (line !== JSON.stringify({ subject, emailNormalized })) {
    throw new Error("Google owner seed stdin payload is not canonical JSON");
  }
  return { subject, emailNormalized };
}

export async function readSaasInstanceSeedStdin(): Promise<Uint8Array> {
  const reader = Bun.stdin.stream().getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > SEED_STDIN_MAX_BYTES) {
        await reader.cancel();
        throw new Error("internal seed stdin payload is missing or oversized");
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const result = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return result;
}
