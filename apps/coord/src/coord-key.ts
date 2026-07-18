// Coord's own ed25519 signing key. Used to mint short-lived browser→worker
// direct JWTs. Loads from disk (OpenSSH unencrypted PEM) or generates fresh.
// Same on-disk path as legacy coord (ssh_ed25519.key) so TOFU pins survive
// the R4.5 cutover. R0.9, R1.1.

import { existsSync, mkdirSync, writeFileSync, readFileSync, chmodSync } from "node:fs";
import { dirname } from "node:path";
import { fingerprintOf, importEd25519PrivkeyPkcs8, signJwt, type SignClaims } from "./jwt.ts";
import { log } from "@roost/shared/log";

export interface CoordKey {
  sign(claims: SignClaims): Promise<string>;
  verifyingKeyB64(): string;
  verifyingKeyKid(): string;
}

// OpenSSH unencrypted ed25519 wire layout constants.
// The private key blob in the "openssh-key-v1" format contains:
//   - check1 (4 bytes) + check2 (4 bytes)
//   - keytype string "ssh-ed25519"
//   - pubkey bytes (4-byte len prefix + 32 bytes)
//   - privkey bytes (4-byte len prefix + 64 bytes: priv||pub)
// We need only the 64-byte private seed to construct PKCS#8.

const OPENSSH_MAGIC = "openssh-key-v1";
const PKCS8_ED25519_PREFIX = new Uint8Array([
  0x30, 0x2e,        // SEQUENCE (46 bytes)
  0x02, 0x01, 0x00,  // INTEGER 0 (version)
  0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70,  // AlgorithmIdentifier (Ed25519 OID)
  0x04, 0x22, 0x04, 0x20,  // OCTET STRING wrapping OCTET STRING (32 bytes)
]);

function parseOpenSshEd25519(pem: string): { privSeed: Uint8Array; pubRaw: Uint8Array } {
  // Strip PEM armor.
  const b64 = pem
    .split("\n")
    .filter((l) => l && !l.startsWith("-----"))
    .join("");
  const raw = new Uint8Array(Buffer.from(b64, "base64"));

  // Verify magic bytes.
  const magic = new TextDecoder().decode(raw.subarray(0, OPENSSH_MAGIC.length));
  if (magic !== OPENSSH_MAGIC) throw new Error("not openssh-key-v1 format");

  // After magic+null byte, skip: ciphername, kdfname, kdfoptions, number-of-keys.
  // Walk the string fields: ciphername, kdfname, kdfoptions, nkeys.
  let pos = OPENSSH_MAGIC.length + 1; // skip magic + 0x00

  function readU32(): number {
    const v = (raw[pos]! << 24) | (raw[pos + 1]! << 16) | (raw[pos + 2]! << 8) | raw[pos + 3]!;
    pos += 4;
    return v >>> 0;
  }
  function readString(): Uint8Array {
    const len = readU32();
    const s = raw.subarray(pos, pos + len);
    pos += len;
    return s;
  }

  readString(); // ciphername
  readString(); // kdfname
  readString(); // kdfoptions
  readU32();    // number-of-keys = 1

  readString(); // public key block (skip; we derive from private)

  // Private key block.
  const privBlock = readString();
  let pp = 0;

  function readU32p(): number {
    const v = (privBlock[pp]! << 24) | (privBlock[pp + 1]! << 16) | (privBlock[pp + 2]! << 8) | privBlock[pp + 3]!;
    pp += 4;
    return v >>> 0;
  }
  function readStringp(): Uint8Array {
    const len = readU32p();
    const s = privBlock.subarray(pp, pp + len);
    pp += len;
    return s;
  }

  readU32p(); readU32p(); // check1, check2 (must match; we trust the file)
  const keytype = new TextDecoder().decode(readStringp());
  if (keytype !== "ssh-ed25519") throw new Error(`expected ssh-ed25519 got ${keytype}`);

  const pubRaw = readStringp(); // 32-byte pubkey
  const privAndPub = readStringp(); // 64 bytes: 32-byte seed || 32-byte pub

  return { privSeed: privAndPub.subarray(0, 32), pubRaw: pubRaw.subarray(0, 32) };
}

function seedToPkcs8(seed: Uint8Array): Uint8Array {
  // PKCS#8 for Ed25519 = 16-byte DER prefix + 32-byte seed.
  const out = new Uint8Array(PKCS8_ED25519_PREFIX.length + 32);
  out.set(PKCS8_ED25519_PREFIX);
  out.set(seed.subarray(0, 32), PKCS8_ED25519_PREFIX.length);
  return out;
}

function generateOpenSshEd25519(): { pem: string; pubRaw: Uint8Array; privSeed: Uint8Array } {
  // Generate via WebCrypto, export, convert to OpenSSH format.
  // Synchronous workaround: use Bun's built-in generateKeyPairSync from
  // node:crypto since we're at module init time. Bun supports this.
  const { generateKeyPairSync } = require("node:crypto") as typeof import("node:crypto");
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");

  const privDer = privateKey.export({ type: "pkcs8", format: "der" }) as Buffer;
  const pubDer = publicKey.export({ type: "spki", format: "der" }) as Buffer;

  const privSeed = new Uint8Array(privDer.subarray(privDer.length - 32));
  const pubRaw = new Uint8Array(pubDer.subarray(pubDer.length - 32));

  // Build OpenSSH format.
  function u32(n: number): Uint8Array {
    const b = new Uint8Array(4);
    new DataView(b.buffer).setUint32(0, n, false);
    return b;
  }
  function str(s: Uint8Array | string): Uint8Array {
    const bytes = typeof s === "string" ? new TextEncoder().encode(s) : s;
    const out = new Uint8Array(4 + bytes.length);
    out.set(u32(bytes.length));
    out.set(bytes, 4);
    return out;
  }
  function concat(...arrs: Uint8Array[]): Uint8Array {
    const total = arrs.reduce((acc, a) => acc + a.length, 0);
    const out = new Uint8Array(total);
    let off = 0;
    for (const a of arrs) { out.set(a, off); off += a.length; }
    return out;
  }

  // check1 = check2 = 4 random bytes.
  const check = new Uint8Array(4);
  crypto.getRandomValues(check);

  const privKeyBlob = concat(
    check, check,
    str("ssh-ed25519"),
    str(pubRaw),
    str(concat(privSeed, pubRaw)),  // 64-byte privAndPub
    str("roost-coord"),              // comment
  );

  const pubBlock = concat(str("ssh-ed25519"), str(pubRaw));

  const body = concat(
    str("none"),       // ciphername
    str("none"),       // kdfname
    str(new Uint8Array(0)), // kdfoptions
    u32(1),            // nkeys
    str(pubBlock),
    str(privKeyBlob),
  );

  const magicBytes = concat(new TextEncoder().encode(OPENSSH_MAGIC), new Uint8Array([0]));
  const full = concat(magicBytes, body);
  const b64 = Buffer.from(full).toString("base64").match(/.{1,70}/g)!.join("\n");
  const pem = `-----BEGIN OPENSSH PRIVATE KEY-----\n${b64}\n-----END OPENSSH PRIVATE KEY-----\n`;

  return { pem, pubRaw, privSeed };
}

export async function loadOrCreateCoordKey(keyPath: string): Promise<CoordKey> {
  let privSeed: Uint8Array;
  let pubRaw: Uint8Array;

  if (existsSync(keyPath)) {
    const pem = readFileSync(keyPath, "utf8");
    if (pem.includes("OPENSSH PRIVATE KEY")) {
      ({ privSeed, pubRaw } = parseOpenSshEd25519(pem));
    } else {
      throw new Error(`coord key at ${keyPath}: unrecognized format`);
    }
  } else {
    const gen = generateOpenSshEd25519();
    privSeed = gen.privSeed;
    pubRaw = gen.pubRaw;
    try { mkdirSync(dirname(keyPath), { recursive: true }); } catch { /* exists */ }
    writeFileSync(keyPath, gen.pem, { mode: 0o600 });
    try { chmodSync(keyPath, 0o600); } catch { /* best-effort */ }
    try { writeFileSync(`${keyPath}.pub`, `ssh-ed25519 ${Buffer.from(pubRaw).toString("base64")} roost-coord\n`); } catch { /* ignore */ }
  }

  const pkcs8Der = seedToPkcs8(privSeed);
  const cryptoKey = await importEd25519PrivkeyPkcs8(pkcs8Der);
  const fp = await fingerprintOf(pubRaw);
  const pubB64 = Buffer.from(pubRaw).toString("base64url");

  log.info("coord-key", "loaded", { fp, path: keyPath });

  return {
    async sign(claims: SignClaims): Promise<string> {
      return signJwt(claims, cryptoKey, fp);
    },
    verifyingKeyB64(): string {
      return pubB64;
    },
    verifyingKeyKid(): string {
      return fp;
    },
  };
}
