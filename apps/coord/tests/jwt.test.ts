// JWT sign + verify round-trip. Expired token rejected. Wrong aud rejected.
// Uses real WebCrypto (Bun's native crypto.subtle). R5.2 prerequisite.

import { describe, test, expect, beforeAll } from "bun:test";
import { Database } from "bun:sqlite";
import { Kysely } from "kysely";
import { BunSqliteDialect } from "kysely-bun-sqlite/dist/index.js";
import {
  fingerprintOf,
  importEd25519PrivkeyPkcs8,
  signJwt,
  verifyJwt,
  newJwtCache,
} from "../src/jwt.ts";
import type { DB } from "../src/db/schema.ts";

// PKCS#8 DER for a test ed25519 key (generated once; hardcoded for speed).
// Real key: DO NOT USE IN PRODUCTION. Generated via:
//   const {privateKey} = await crypto.subtle.generateKey({name:"Ed25519"},true,["sign","verify"])
//   const raw = await crypto.subtle.exportKey("pkcs8", privateKey)
// Then extracted as base64 and hardcoded.

let testPrivKey: CryptoKey;
let testPubRaw: Uint8Array;
let testFp: string;
let db: Kysely<DB>;

const SPKI_PREFIX = new Uint8Array([
  0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
]);
const PKCS8_PREFIX = new Uint8Array([
  0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
]);

beforeAll(async () => {
  // Generate a fresh ed25519 keypair for tests.
  const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);

  // Export PKCS#8 private key.
  const pkcs8 = await crypto.subtle.exportKey("pkcs8", pair.privateKey);
  testPrivKey = await importEd25519PrivkeyPkcs8(new Uint8Array(pkcs8));

  // Extract raw 32-byte pubkey from SPKI export.
  const spki = await crypto.subtle.exportKey("spki", pair.publicKey);
  testPubRaw = new Uint8Array(spki).subarray(SPKI_PREFIX.length);
  testFp = await fingerprintOf(testPubRaw);

  // In-memory SQLite DB with authorized_keys table.
  const sqlite = new Database(":memory:");
  sqlite.exec(`CREATE TABLE authorized_keys (fingerprint TEXT PRIMARY KEY, public_key BLOB NOT NULL, label TEXT NOT NULL, added_at INTEGER NOT NULL)`);
  sqlite.exec(`INSERT INTO authorized_keys VALUES ('${testFp}', X'${Buffer.from(testPubRaw).toString("hex")}', 'test-key', ${Date.now()})`);

  db = new Kysely<DB>({ dialect: new BunSqliteDialect({ database: sqlite }) });
});

// ─── round-trip ────────────────────────────────────────────────────────

describe("JWT round-trip", () => {
  test("sign + verify returns correct fingerprint + label", async () => {
    const nowSecs = Math.floor(Date.now() / 1000);
    const token = await signJwt(
      { aud: "roost-coordinator", sub: "test", iat: nowSecs },
      testPrivKey,
      testFp,
    );

    const caller = await verifyJwt(token, {
      db,
      cache: newJwtCache(),
      jwtMaxAgeSecs: 300,
    });

    expect(caller.fingerprint).toBe(testFp);
    expect(caller.label).toBe("test-key");
  });

  test("cache hit: second verify uses cache", async () => {
    const nowSecs = Math.floor(Date.now() / 1000);
    const cache = newJwtCache();
    const token = await signJwt(
      { aud: "roost-coordinator", sub: "test", iat: nowSecs },
      testPrivKey,
      testFp,
    );
    await verifyJwt(token, { db, cache, jwtMaxAgeSecs: 300 });
    // Second call should hit cache (no DB call needed).
    const caller2 = await verifyJwt(token, { db, cache, jwtMaxAgeSecs: 300 });
    expect(caller2.fingerprint).toBe(testFp);
  });
});

// ─── rejection cases ───────────────────────────────────────────────────

describe("JWT rejection", () => {
  test("expired token rejected", async () => {
    const expiredIat = Math.floor(Date.now() / 1000) - 400; // > 300s old
    const token = await signJwt(
      { aud: "roost-coordinator", sub: "test", iat: expiredIat },
      testPrivKey,
      testFp,
    );
    await expect(
      verifyJwt(token, { db, cache: newJwtCache(), jwtMaxAgeSecs: 300 }),
    ).rejects.toThrow(/too old/);
  });

  test("wrong audience rejected", async () => {
    const nowSecs = Math.floor(Date.now() / 1000);
    const token = await signJwt(
      { aud: "worker-direct", sub: "test", iat: nowSecs },
      testPrivKey,
      testFp,
    );
    await expect(
      verifyJwt(token, { db, cache: newJwtCache(), jwtMaxAgeSecs: 300 }),
    ).rejects.toThrow(/wrong aud/);
  });

  test("tampered signature rejected", async () => {
    const nowSecs = Math.floor(Date.now() / 1000);
    const token = await signJwt(
      { aud: "roost-coordinator", sub: "test", iat: nowSecs },
      testPrivKey,
      testFp,
    );
    // Replace the entire signature segment with all-zero bytes (invalid).
    const parts = token.split(".");
    parts[2] = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const tampered = parts.join(".");
    await expect(
      verifyJwt(tampered, { db, cache: newJwtCache(), jwtMaxAgeSecs: 300 }),
    ).rejects.toThrow(/signature invalid|bad jwt/);
  });

  test("token from the future rejected", async () => {
    const futureIat = Math.floor(Date.now() / 1000) + 60; // 60s in future
    const token = await signJwt(
      { aud: "roost-coordinator", sub: "test", iat: futureIat },
      testPrivKey,
      testFp,
    );
    await expect(
      verifyJwt(token, { db, cache: newJwtCache(), jwtMaxAgeSecs: 300 }),
    ).rejects.toThrow(/future/);
  });

  test("unknown kid rejected", async () => {
    const unknownFp = "f".repeat(64);
    const nowSecs = Math.floor(Date.now() / 1000);
    const token = await signJwt(
      { aud: "roost-coordinator", sub: "test", iat: nowSecs },
      testPrivKey,
      unknownFp,  // kid not in authorized_keys
    );
    await expect(
      verifyJwt(token, { db, cache: newJwtCache(), jwtMaxAgeSecs: 300 }),
    ).rejects.toThrow(/unknown kid/);
  });
});
