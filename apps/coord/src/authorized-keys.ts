// authorized_keys file import + fingerprint helper.
// Parses ssh-ed25519 lines, upserts into authorized_keys table.
// Mirrors legacy lib/authorizedKeys.ts but uses Kysely + Bun's WebCrypto.

import { readFileSync } from "node:fs";
import { fingerprintOf } from "@roost/shared/fingerprint";
import type { KyselyDB } from "./db/connection.ts";

export interface ParsedKey {
  pubkey: Uint8Array;  // raw 32 bytes
  label: string;
}

// OpenSSH wire: <u32 11><"ssh-ed25519"><u32 32><32-byte key>
export function parseSshEd25519Line(line: string): ParsedKey | null {
  const t = line.trim();
  if (!t || t.startsWith("#")) return null;
  const parts = t.split(/\s+/);
  if (parts.length < 2 || parts[0] !== "ssh-ed25519") return null;
  let raw: Uint8Array;
  try {
    raw = new Uint8Array(Buffer.from(parts[1]!, "base64"));
  } catch {
    return null;
  }
  const minLen = 4 + 11 + 4 + 32;
  if (raw.length < minLen) return null;
  const keyStart = 4 + 11 + 4;
  const label = parts.slice(2).join(" ") || "(no label)";
  return { pubkey: raw.subarray(keyStart, keyStart + 32), label };
}

// Accept either raw 32-byte b64/b64url (browser/worker tRPC payloads) OR
// the SSH wire-format b64 (legacy authorized_keys imports).
// Browsers and Bun workers both send raw 32 bytes; the SSH wire layer
// is only used by file-import paths now.
export function decodeEd25519Pubkey(b64: string): Uint8Array | null {
  const normalized = b64.replace(/-/g, "+").replace(/_/g, "/");
  let decoded: Uint8Array;
  try {
    decoded = new Uint8Array(Buffer.from(normalized, "base64"));
  } catch {
    return null;
  }
  if (decoded.length === 32) return decoded;
  const type = new TextEncoder().encode("ssh-ed25519");
  const expectedLength = 4 + type.length + 4 + 32;
  if (decoded.length !== expectedLength) return null;
  const view = new DataView(decoded.buffer, decoded.byteOffset, decoded.byteLength);
  if (view.getUint32(0, false) !== type.length) return null;
  for (let index = 0; index < type.length; index++) {
    if (decoded[4 + index] !== type[index]) return null;
  }
  const keyLengthOffset = 4 + type.length;
  if (view.getUint32(keyLengthOffset, false) !== 32) return null;
  return decoded.subarray(keyLengthOffset + 4);
}

export async function isAuthorizedKeyRevoked(db: KyselyDB, fingerprint: string): Promise<boolean> {
  const row = await db.selectFrom("authorized_key_revocations")
    .select("fingerprint")
    .where("fingerprint", "=", fingerprint)
    .executeTakeFirst();
  return row !== undefined;
}

export async function importAuthorizedKeys(db: KyselyDB, filePath: string): Promise<number> {
  const contents = readFileSync(filePath, "utf8");
  const now = Date.now();
  const accounts = await db.selectFrom("accounts")
    .select(["id", "status"])
    .limit(2)
    .execute();
  const browserAccountId = accounts.length === 1 && accounts[0]?.status === "active"
    ? accounts[0].id
    : null;
  let count = 0;
  for (const line of contents.split(/\r?\n/)) {
    const parsed = parseSshEd25519Line(line);
    if (!parsed) continue;
    const fp = await fingerprintOf(parsed.pubkey);
    if (await isAuthorizedKeyRevoked(db, fp)) continue;
    await db.transaction().execute(async (trx) => {
      await trx
        .insertInto("authorized_keys")
        .values({ fingerprint: fp, public_key: parsed.pubkey, label: parsed.label, added_at: now })
        .onConflict((oc) => oc.column("fingerprint").doUpdateSet({ label: parsed.label }))
        .execute();
      if (browserAccountId !== null) {
        const worker = await trx.selectFrom("workers")
          .select("fp")
          .where("fp", "=", fp)
          .executeTakeFirst();
        if (!worker) {
          await trx.insertInto("account_devices").values({
            fingerprint: fp,
            account_id: browserAccountId,
            added_at_ms: now,
            last_seen_at_ms: now,
          }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({
            last_seen_at_ms: now,
          })).execute();
        }
      }
    });
    count++;
  }
  return count;
}
