// Worker JWT minting. Signs EdDSA JWTs (aud=roost-coordinator) using the
// worker's ed25519 private key at workerKeyPath from config. Uses Bun's
// native crypto.subtle (WebCrypto Ed25519) — no external crypto lib.
// Callers: coord-client.ts (Authorization header on every coord RPC).

import { existsSync, readFileSync } from "node:fs";
import { log } from "@roost/shared/log";
import { durableWriteFile } from "@roost/shared/durability";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { windowsApplyAccountDacl } from "@roost/shared/windows-helper";

// PKCS8 DER prefix for a raw 32-byte Ed25519 seed. crypto.subtle.importKey
// only accepts pkcs8/jwk/spki — prepend this to the seed to import it.
const PKCS8_ED25519_PREFIX = new Uint8Array([
  0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70,
  0x04, 0x22, 0x04, 0x20,
]);

function seedToPkcs8(seed: Uint8Array): Uint8Array {
  const out = new Uint8Array(PKCS8_ED25519_PREFIX.length + 32);
  out.set(PKCS8_ED25519_PREFIX);
  out.set(seed.subarray(0, 32), PKCS8_ED25519_PREFIX.length);
  return out;
}

async function importSigningKey(seed: Uint8Array): Promise<CryptoKey> {
  const pkcs8 = seedToPkcs8(seed);
  return crypto.subtle.importKey(
    "pkcs8",
    pkcs8.buffer.slice(pkcs8.byteOffset, pkcs8.byteOffset + pkcs8.byteLength) as ArrayBuffer,
    { name: "Ed25519" },
    false,
    ["sign"],
  );
}

// ─── OpenSSH private key parsing ─────────────────────────────────────
// The key is stored as "-----BEGIN OPENSSH PRIVATE KEY-----" PEM.
// We extract the raw 32-byte ed25519 seed (private scalar) from it.

const OPENSSH_MAGIC = Buffer.from("openssh-key-v1\0", "ascii");

function parseOpenSshEd25519(pem: string): { privSeed: Uint8Array; pubKey: Uint8Array } {
  const b64 = pem
    .replace(/-----[^-]+-----/g, "")
    .replace(/\s+/g, "");
  const der = Buffer.from(b64, "base64");

  if (!der.subarray(0, 15).equals(OPENSSH_MAGIC.subarray(0, 15))) {
    throw new Error("jwt: not an openssh key");
  }

  // Scan forward: the format is documented in PROTOCOL.key (OpenSSH).
  // We skip to the private key block by finding the 64-byte private+public pair.
  // The seed is at offset: magic(15) + null(1) + ciphername-len(4) + "none"(4) +
  //   kdfname-len(4) + "none"(4) + kdf-options-len(4) + 0(0) + nkeys(4) + 1(4)
  //   + pubkey-block-len(4) + ssh-ed25519-tag-len(4) + "ssh-ed25519"(11) +
  //   pubkey-len(4) + pubkey(32) + privkey-block-len(4) + checkint1(4) + checkint2(4)
  //   + privkey-tag-len(4) + "ssh-ed25519"(11) + pubkey-repeat-len(4) + pubkey-repeat(32)
  //   + privkey-len(4) + privkey(64)  ← first 32 bytes = seed, next 32 = pubkey
  //
  // Rather than a full parser, we find the 64-byte private key blob by scanning
  // for the 32-byte pubkey that appears twice. Simplest approach that works for
  // the single-key no-passphrase case this worker uses.

  // Find "ssh-ed25519" in der.
  const tag = Buffer.from("ssh-ed25519", "ascii");
  let idx = der.indexOf(tag, 16);
  if (idx === -1) throw new Error("jwt: ssh-ed25519 tag not found");
  // Skip past tag + 4-byte length + 32-byte pubkey (first occurrence).
  idx += tag.length + 4;
  const pubKey = der.subarray(idx, idx + 32);
  // Advance past pubkey, find second occurrence (inside private block).
  idx = der.indexOf(tag, idx + 32);
  if (idx === -1) throw new Error("jwt: private block tag not found");
  idx += tag.length + 4; // skip tag + pubkey-len-field
  idx += 32;             // skip second pubkey copy
  // Now at privkey-len (4 bytes) + privkey (64 bytes). Seed is first 32.
  const privLen = der.readUInt32BE(idx);
  if (privLen !== 64) throw new Error(`jwt: unexpected privkey len ${privLen}`);
  const privSeed = der.subarray(idx + 4, idx + 4 + 32);
  return { privSeed: Uint8Array.from(privSeed), pubKey: Uint8Array.from(pubKey) };
}

// ─── JWT minting ──────────────────────────────────────────────────────

interface LoadedKey {
  seed: Uint8Array;
  pubKey: Uint8Array;
  fingerprint: string;
  signingKey: CryptoKey;
}

let _key: LoadedKey | null = null;

export async function loadWorkerKey(keyPath: string): Promise<LoadedKey> {
  if (_key) return _key;

  // First-boot: generate fresh OpenSSH ed25519 keypair if missing or
  // unparseable (legacy Rust worker stored raw 32-byte seeds in the
  // same path which v2 can't read; treat any parse failure as
  // missing-key + regenerate).
  let pem: string;
  let parsed: { privSeed: Uint8Array; pubKey: Uint8Array } | null = null;
  if (existsSync(keyPath)) {
    pem = readFileSync(keyPath, "utf8");
    try {
      parsed = parseOpenSshEd25519(pem);
    } catch {
      log.warn("jwt", "existing key not openssh format — regenerating", { keyPath });
      parsed = null;
    }
  }
  if (!parsed) {
    parsed = await generateAndWriteKey(keyPath);
  }
  if (process.platform === "win32" && process.env.ROOST_SERVICE_ACCOUNT) {
    await windowsApplyAccountDacl(keyPath, process.env.ROOST_SERVICE_ACCOUNT);
  }

  const fingerprint = await fingerprintOf(parsed.pubKey);
  const signingKey = await importSigningKey(parsed.privSeed);
  _key = { seed: parsed.privSeed, pubKey: parsed.pubKey, fingerprint, signingKey };
  log.info("jwt", "worker key loaded", { fingerprint });
  return _key;
}

// Generate a fresh OpenSSH-format ed25519 key + write to disk. Uses
// crypto.subtle.generateKey then exports the raw seed (last 32 bytes of
// the PKCS8 DER) + raw pubkey.
async function generateAndWriteKey(keyPath: string): Promise<{ privSeed: Uint8Array; pubKey: Uint8Array }> {
  const pair = (await crypto.subtle.generateKey(
    { name: "Ed25519" },
    true,
    ["sign", "verify"],
  )) as CryptoKeyPair;
  const pkcs8 = new Uint8Array(await crypto.subtle.exportKey("pkcs8", pair.privateKey));
  const privSeed = pkcs8.subarray(pkcs8.length - 32);
  const pubKey = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  const pem = encodeOpenSshEd25519(privSeed, pubKey);
  await durableWriteFile(keyPath, pem, { mode: 0o600, privateDacl: true });
  return { privSeed: Uint8Array.from(privSeed), pubKey };
}

// Encode a 32-byte seed + 32-byte pubkey into an OpenSSH PEM string.
// PROTOCOL.key format with no encryption (cipher=none, kdf=none).
function encodeOpenSshEd25519(privSeed: Uint8Array, pubKey: Uint8Array): string {
  const magic = Buffer.from("openssh-key-v1\0", "ascii");
  const sshStr = (s: string) => {
    const b = Buffer.from(s, "ascii");
    const len = Buffer.alloc(4);
    len.writeUInt32BE(b.length);
    return Buffer.concat([len, b]);
  };
  const sshBytes = (b: Uint8Array) => {
    const lenBuf = Buffer.alloc(4);
    lenBuf.writeUInt32BE(b.length);
    return Buffer.concat([lenBuf, Buffer.from(b)]);
  };
  const sshU32 = (n: number) => {
    const b = Buffer.alloc(4);
    b.writeUInt32BE(n);
    return b;
  };
  const sshTag = sshStr("ssh-ed25519");

  const pubBlock = Buffer.concat([sshTag, sshBytes(pubKey)]);
  const sshPubBlock = sshBytes(pubBlock);

  const checkInt = Buffer.alloc(4);
  // Use any consistent 32-bit value for the integrity check pair.
  checkInt.writeUInt32BE(0x12345678);
  const privKey64 = Buffer.concat([Buffer.from(privSeed), Buffer.from(pubKey)]);
  const privInner = Buffer.concat([
    checkInt, checkInt,
    sshTag,
    sshBytes(pubKey),
    sshBytes(privKey64),
    sshStr(""), // comment
  ]);
  // Pad to 8-byte multiple per OpenSSH spec.
  const pad = (8 - (privInner.length % 8)) % 8;
  const padding = Buffer.alloc(pad);
  for (let i = 0; i < pad; i++) padding[i] = i + 1;
  const sshPrivBlock = sshBytes(Buffer.concat([privInner, padding]));

  const body = Buffer.concat([
    magic,
    sshStr("none"), // cipher
    sshStr("none"), // kdfname
    sshStr(""),     // kdfoptions
    sshU32(1),      // number of keys
    sshPubBlock,
    sshPrivBlock,
  ]);

  const b64 = body.toString("base64").match(/.{1,70}/g)!.join("\n");
  return `-----BEGIN OPENSSH PRIVATE KEY-----\n${b64}\n-----END OPENSSH PRIVATE KEY-----\n`;
}

/** Base64url encode (no padding). */
function b64url(buf: Uint8Array): string {
  return Buffer.from(buf).toString("base64url");
}

/** Mint a short-lived EdDSA JWT for the given audience. */
export async function mintJwt(key: LoadedKey, aud: "roost-coordinator" | "worker-direct", ttlSecs = 300): Promise<string> {
  const now = Math.floor(Date.now() / 1000);
  // kid identifies which authorized_keys row to verify against. Coord's
  // verifyJwt rejects any JWT without it.
  const header = { alg: "EdDSA", typ: "JWT", kid: key.fingerprint };
  const payload = { sub: key.fingerprint, iat: now, exp: now + ttlSecs, aud };
  const headerB64 = b64url(Buffer.from(JSON.stringify(header)));
  const payloadB64 = b64url(Buffer.from(JSON.stringify(payload)));
  const signingInput = `${headerB64}.${payloadB64}`;
  const msgBytes = new TextEncoder().encode(signingInput);
  const sigBytes = new Uint8Array(
    await crypto.subtle.sign({ name: "Ed25519" }, key.signingKey, msgBytes),
  );
  return `${signingInput}.${b64url(sigBytes)}`;
}
