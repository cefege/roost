// Managed E2Es consume the image and Docker network owned by `roost test managed`.
// This fixture rejects direct opt-in runs that would otherwise build, guess, or delete shared resources.
// Scenario files retain ownership only of their containers and temporary host directories.

import { generateKeyPairSync, type KeyObject } from "node:crypto";
import { chmodSync, writeFileSync } from "node:fs";

const IMMUTABLE_IMAGE_ID_RE = /^sha256:[0-9a-f]{64}$/;
const GIT_SHA_RE = /^[0-9a-f]{40}$/i;

interface ManagedE2eResources {
  imageId: string;
  network: string;
  gitSha: string;
}

function uint32(value: number): Buffer {
  const bytes = Buffer.allocUnsafe(4);
  bytes.writeUInt32BE(value, 0);
  return bytes;
}

function writeEd25519VerificationKeyFixture(path: string): KeyObject {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const publicJwk = publicKey.export({ format: "jwk" });
  if (typeof publicJwk.x !== "string") throw new Error("generated Ed25519 fixture key is invalid");
  const rawPublicKey = Buffer.from(publicJwk.x, "base64url");
  if (rawPublicKey.byteLength !== 32 || rawPublicKey.toString("base64url") !== publicJwk.x) {
    throw new Error("generated Ed25519 fixture key is invalid");
  }
  const algorithm = Buffer.from("ssh-ed25519", "ascii");
  const wire = Buffer.concat([
    uint32(algorithm.byteLength),
    algorithm,
    uint32(rawPublicKey.byteLength),
    rawPublicKey,
  ]);
  writeFileSync(path, `ssh-ed25519 ${wire.toString("base64")} roost-saas-auth\n`, { mode: 0o644 });
  chmodSync(path, 0o644);
  return privateKey;
}

function requiredManagedE2eResources(
  env: NodeJS.ProcessEnv = process.env,
): ManagedE2eResources {
  const imageId = env.ROOST_SAAS_E2E_IMAGE;
  if (!imageId) {
    throw new Error("managed E2E requires ROOST_SAAS_E2E_IMAGE from `bun run test:managed`");
  }
  if (!IMMUTABLE_IMAGE_ID_RE.test(imageId)) {
    throw new Error("ROOST_SAAS_E2E_IMAGE must be an immutable sha256 image ID");
  }
  const network = env.ROOST_SAAS_E2E_NETWORK;
  if (!network) {
    throw new Error("managed E2E requires ROOST_SAAS_E2E_NETWORK from `bun run test:managed`");
  }
  if (network.trim() !== network || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(network)) {
    throw new Error("ROOST_SAAS_E2E_NETWORK must be a Docker network name");
  }
  const gitSha = env.ROOST_SAAS_E2E_GIT_SHA;
  if (!gitSha) {
    throw new Error("managed E2E requires ROOST_SAAS_E2E_GIT_SHA from `bun run test:managed`");
  }
  if (!GIT_SHA_RE.test(gitSha)) {
    throw new Error("ROOST_SAAS_E2E_GIT_SHA must be exactly 40 hexadecimal characters");
  }
  return { imageId, network, gitSha };
}

export { requiredManagedE2eResources, writeEd25519VerificationKeyFixture };
export type { ManagedE2eResources };
