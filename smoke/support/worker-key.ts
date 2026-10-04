// The harness API key: an OpenSSH ed25519 key file the stack generates on first
// use, authorizes in the coordinator database, and signs EdDSA JWTs with
// (aud=roost-coordinator). Called by the terminal stack and the CLI-bridge spec;
// depends only on WebCrypto Ed25519 and node:fs.

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { fingerprintOf } from "./fingerprint.ts";

// PKCS8 DER prefix for a raw 32-byte Ed25519 seed. crypto.subtle.importKey
// only accepts pkcs8/jwk/spki — prepend this to the seed to import it.
const PKCS8_ED25519_PREFIX = new Uint8Array([
  0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70,
  0x04, 0x22, 0x04, 0x20,
]);

const OPENSSH_MAGIC = Buffer.from("openssh-key-v1\0", "ascii");
const SSH_ED25519_TAG = Buffer.from("ssh-ed25519", "ascii");

export interface LoadedKey {
  seed: Uint8Array;
  pubKey: Uint8Array;
  fingerprint: string;
  signingKey: CryptoKey;
}

/** Read the key at `keyPath`, generating and writing a fresh one when it is
 *  absent or not an OpenSSH ed25519 key. */
export async function loadWorkerKey(keyPath: string): Promise<LoadedKey> {
  let parsed: { privSeed: Uint8Array; pubKey: Uint8Array } | null = null;
  if (existsSync(keyPath)) {
    try {
      parsed = parseOpenSshEd25519(readFileSync(keyPath, "utf8"));
    } catch {
      parsed = null;
    }
  }
  parsed ??= await generateAndWriteKey(keyPath);
  return {
    seed: parsed.privSeed,
    pubKey: parsed.pubKey,
    fingerprint: await fingerprintOf(parsed.pubKey),
    signingKey: await importSigningKey(parsed.privSeed),
  };
}

/** Mint a short-lived EdDSA JWT for the given audience. */
export async function mintJwt(
  key: LoadedKey,
  aud: "roost-coordinator" | "worker-direct",
  ttlSecs = 300,
): Promise<string> {
  const now = Math.floor(Date.now() / 1000);
  // kid names the authorized_keys row the coordinator verifies against; a JWT
  // without it is rejected.
  const header = { alg: "EdDSA", typ: "JWT", kid: key.fingerprint };
  const payload = { sub: key.fingerprint, iat: now, exp: now + ttlSecs, aud };
  const signingInput = `${b64url(Buffer.from(JSON.stringify(header)))}.${b64url(Buffer.from(JSON.stringify(payload)))}`;
  const signature = new Uint8Array(
    await crypto.subtle.sign({ name: "Ed25519" }, key.signingKey, new TextEncoder().encode(signingInput)),
  );
  return `${signingInput}.${b64url(signature)}`;
}

function importSigningKey(seed: Uint8Array): Promise<CryptoKey> {
  const pkcs8 = new Uint8Array(PKCS8_ED25519_PREFIX.length + 32);
  pkcs8.set(PKCS8_ED25519_PREFIX);
  pkcs8.set(seed.subarray(0, 32), PKCS8_ED25519_PREFIX.length);
  return crypto.subtle.importKey("pkcs8", pkcs8.buffer as ArrayBuffer, { name: "Ed25519" }, false, ["sign"]);
}

/**
 * Extract the raw seed and public key of a single-key, unencrypted OpenSSH
 * ed25519 private key (PROTOCOL.key). The public key appears twice — in the
 * public block and again inside the private block — and the 64-byte private
 * blob (seed ‖ pubkey) follows the second copy.
 */
function parseOpenSshEd25519(pem: string): { privSeed: Uint8Array; pubKey: Uint8Array } {
  const der = Buffer.from(pem.replace(/-----[^-]+-----/g, "").replace(/\s+/g, ""), "base64");
  if (!der.subarray(0, 15).equals(OPENSSH_MAGIC.subarray(0, 15))) {
    throw new Error("worker key: not an openssh key");
  }
  let offset = der.indexOf(SSH_ED25519_TAG, 16);
  if (offset === -1) throw new Error("worker key: ssh-ed25519 tag not found");
  offset += SSH_ED25519_TAG.length + 4;
  const pubKey = der.subarray(offset, offset + 32);
  offset = der.indexOf(SSH_ED25519_TAG, offset + 32);
  if (offset === -1) throw new Error("worker key: private block tag not found");
  offset += SSH_ED25519_TAG.length + 4 + 32;
  const privLen = der.readUInt32BE(offset);
  if (privLen !== 64) throw new Error(`worker key: unexpected private key length ${privLen}`);
  const privSeed = der.subarray(offset + 4, offset + 4 + 32);
  return { privSeed: Uint8Array.from(privSeed), pubKey: Uint8Array.from(pubKey) };
}

async function generateAndWriteKey(keyPath: string): Promise<{ privSeed: Uint8Array; pubKey: Uint8Array }> {
  const pair = (await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"])) as CryptoKeyPair;
  const pkcs8 = new Uint8Array(await crypto.subtle.exportKey("pkcs8", pair.privateKey));
  // The raw seed is the last 32 bytes of the PKCS8 DER.
  const privSeed = Uint8Array.from(pkcs8.subarray(pkcs8.length - 32));
  const pubKey = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  writeFileSync(keyPath, encodeOpenSshEd25519(privSeed, pubKey), { mode: 0o600 });
  return { privSeed, pubKey };
}

/** Encode a seed and public key as an unencrypted OpenSSH private key PEM. */
function encodeOpenSshEd25519(privSeed: Uint8Array, pubKey: Uint8Array): string {
  const sshU32 = (value: number) => {
    const bytes = Buffer.alloc(4);
    bytes.writeUInt32BE(value);
    return bytes;
  };
  const sshBytes = (bytes: Uint8Array) => Buffer.concat([sshU32(bytes.length), Buffer.from(bytes)]);
  const sshStr = (text: string) => sshBytes(Buffer.from(text, "ascii"));
  const sshTag = sshStr("ssh-ed25519");

  const publicBlock = sshBytes(Buffer.concat([sshTag, sshBytes(pubKey)]));
  // The check-int pair only has to match itself.
  const checkInt = sshU32(0x12345678);
  const privateInner = Buffer.concat([
    checkInt,
    checkInt,
    sshTag,
    sshBytes(pubKey),
    sshBytes(Buffer.concat([Buffer.from(privSeed), Buffer.from(pubKey)])),
    sshStr(""),
  ]);
  const padLength = (8 - (privateInner.length % 8)) % 8;
  const padding = Buffer.from(Array.from({ length: padLength }, (_, index) => index + 1));
  const privateBlock = sshBytes(Buffer.concat([privateInner, padding]));

  const body = Buffer.concat([
    OPENSSH_MAGIC,
    sshStr("none"),
    sshStr("none"),
    sshStr(""),
    sshU32(1),
    publicBlock,
    privateBlock,
  ]);
  const lines = body.toString("base64").match(/.{1,70}/g) ?? [];
  return `-----BEGIN OPENSSH PRIVATE KEY-----\n${lines.join("\n")}\n-----END OPENSSH PRIVATE KEY-----\n`;
}

function b64url(bytes: Uint8Array): string {
  return Buffer.from(bytes).toString("base64url");
}
