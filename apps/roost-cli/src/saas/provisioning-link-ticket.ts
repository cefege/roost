// Verifies coordinator-issued identity-link tickets before provisioning accepts them.
// The private provisioner operation calls this against registry-bound tenant keys.
// Strict claim and key parsing prevents a ticket from crossing tenant boundaries.
import { closeSync, constants, fstatSync, openSync, readFileSync } from "node:fs";
import { jwtVerify } from "jose";
import {
  openSshEd25519PublicKey,
  type GoogleLinkSubmissionDto,
} from "../saas-auth/private-ipc.ts";
import { instanceLayoutFor } from "./layout.ts";
import {
  type SaasRegistry,
  assertCanonicalUuid,
  assertSha256Hex,
} from "./registry.ts";
import type { VerifiedLinkTicket } from "./provisioning-contract.ts";

function readCoordinatorTicketKey(path: string) {
  const descriptor = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = fstatSync(descriptor);
    if (!stat.isFile()
      || (stat.mode & 0o777) !== 0o600
      || stat.uid !== 65_532
      || stat.gid !== 65_532
      || stat.size <= 0
      || stat.size > 64 * 1024) {
      throw new Error("coordinator ticket key file is invalid");
    }
    const pem = readFileSync(descriptor, "utf8");
    const body = pem.split(/\r?\n/u)
      .filter((line) => line.length > 0 && !line.startsWith("-----"))
      .join("");
    const raw = Buffer.from(body, "base64");
    const magic = Buffer.from("openssh-key-v1\0", "utf8");
    if (raw.byteLength <= magic.byteLength || !raw.subarray(0, magic.byteLength).equals(magic)) {
      throw new Error("coordinator ticket key content is invalid");
    }
    let offset = magic.byteLength;
    const readU32 = (): number => {
      if (offset + 4 > raw.byteLength) throw new Error("coordinator ticket key content is invalid");
      const value = raw.readUInt32BE(offset);
      offset += 4;
      return value;
    };
    const readString = (): Buffer => {
      const length = readU32();
      if (offset + length > raw.byteLength) throw new Error("coordinator ticket key content is invalid");
      const value = raw.subarray(offset, offset + length);
      offset += length;
      return value;
    };
    if (readString().toString("utf8") !== "none"
      || readString().toString("utf8") !== "none"
      || readString().byteLength !== 0
      || readU32() !== 1) {
      throw new Error("coordinator ticket key must be one unencrypted Ed25519 key");
    }
    const publicBlock = readString();
    let publicOffset = 0;
    const readPublicString = (): Buffer => {
      if (publicOffset + 4 > publicBlock.byteLength) {
        throw new Error("coordinator ticket public key is invalid");
      }
      const length = publicBlock.readUInt32BE(publicOffset);
      publicOffset += 4;
      if (publicOffset + length > publicBlock.byteLength) {
        throw new Error("coordinator ticket public key is invalid");
      }
      const value = publicBlock.subarray(publicOffset, publicOffset + length);
      publicOffset += length;
      return value;
    };
    if (readPublicString().toString("utf8") !== "ssh-ed25519"
      || readPublicString().byteLength !== 32
      || publicOffset !== publicBlock.byteLength) {
      throw new Error("coordinator ticket public key is invalid");
    }
    return openSshEd25519PublicKey(`ssh-ed25519 ${publicBlock.toString("base64")}`);
  } finally {
    closeSync(descriptor);
  }
}

export async function verifyLinkTicket(
  registry: SaasRegistry,
  submission: GoogleLinkSubmissionDto,
  nowMs: number,
): Promise<VerifiedLinkTicket> {
  const selected = registry.listCoordinators()
    .filter((coordinator) => coordinator.routeKey === submission.routeKey);
  if (selected.length !== 1) throw new Error("link route is unavailable");
  const coordinator = selected[0]!;
  const account = registry.getAccount(coordinator.accountId);
  const key = readCoordinatorTicketKey(instanceLayoutFor(coordinator).coordinatorKeyPath);
  const { payload, protectedHeader } = await jwtVerify(submission.linkTicket, key, {
    algorithms: ["EdDSA"],
    audience: "roost-saas-identity-link",
    requiredClaims: ["sub", "iat", "exp", "jti"],
    currentDate: new Date(nowMs),
  });
  if (protectedHeader.alg !== "EdDSA"
    || (protectedHeader.typ !== undefined && protectedHeader.typ !== "JWT")) {
    throw new Error("link ticket header is invalid");
  }
  const expectedClaims = [
    "account_id",
    "aud",
    "coordinator_id",
    "device_fp",
    "exp",
    "iat",
    "jti",
    "route_key",
    "sub",
  ];
  const actualClaims = Object.keys(payload).sort();
  if (actualClaims.length !== expectedClaims.length
    || actualClaims.some((claim, index) => claim !== expectedClaims[index])) {
    throw new Error("link ticket claims are invalid");
  }
  if (typeof payload.iat !== "number" || !Number.isSafeInteger(payload.iat)
    || typeof payload.exp !== "number" || !Number.isSafeInteger(payload.exp)
    || payload.iat < 0 || payload.exp <= payload.iat || payload.exp > payload.iat + 300) {
    throw new Error("link ticket lifetime is invalid");
  }
  const issuedAtMs = payload.iat * 1_000;
  const expiresAtMs = payload.exp * 1_000;
  if (!Number.isSafeInteger(issuedAtMs) || !Number.isSafeInteger(expiresAtMs)
    || issuedAtMs > nowMs || nowMs >= expiresAtMs) {
    throw new Error("link ticket is stale");
  }
  if (typeof payload.account_id !== "string"
    || typeof payload.coordinator_id !== "string"
    || typeof payload.route_key !== "string"
    || typeof payload.device_fp !== "string"
    || typeof payload.sub !== "string"
    || typeof payload.jti !== "string"
    || payload.aud !== "roost-saas-identity-link") {
    throw new Error("link ticket claims are invalid");
  }
  const accountId = assertCanonicalUuid(payload.account_id, "link ticket account id");
  const coordinatorId = assertCanonicalUuid(payload.coordinator_id, "link ticket coordinator id");
  const routeKey = assertSha256Hex(payload.route_key, "link ticket route key");
  const deviceFingerprint = assertSha256Hex(payload.device_fp, "link ticket device fingerprint");
  const ticketJti = assertCanonicalUuid(payload.jti, "link ticket jti");
  if (payload.sub !== deviceFingerprint
    || accountId !== account.id
    || coordinatorId !== coordinator.id
    || routeKey !== account.routeKey) {
    throw new Error("link ticket registry binding mismatch");
  }
  return {
    ticketJti,
    accountId,
    coordinatorId,
    routeKey,
    deviceFingerprint,
    issuedAtMs,
    expiresAtMs,
  };
}
