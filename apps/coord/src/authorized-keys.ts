// authorized_keys file import + fingerprint helper.
// Parses ssh-ed25519 lines, upserts into authorized_keys table.
// Mirrors legacy lib/authorizedKeys.ts but uses Kysely + Bun's WebCrypto.

import { readFileSync } from "node:fs";
import { fingerprintOf } from "./jwt.ts";
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
  // Try both base64 and base64url. Browser WebCrypto exportKey("raw") returns
  // 32 bytes; b64-encoded that's 44 chars (with padding). SSH-wire is ≥ 68 chars.
  for (const variant of [b64, b64.replace(/-/g, "+").replace(/_/g, "/")]) {
    try {
      const raw = new Uint8Array(Buffer.from(variant, "base64"));
      if (raw.length === 32) return raw;
      // SSH wire fallthrough
      const minLen = 4 + 11 + 4 + 32;
      if (raw.length >= minLen) {
        const keyStart = 4 + 11 + 4;
        return raw.subarray(keyStart, keyStart + 32);
      }
    } catch { /* try next */ }
  }
  return null;
}

export async function importAuthorizedKeys(db: KyselyDB, filePath: string): Promise<number> {
  const contents = readFileSync(filePath, "utf8");
  const now = Date.now();
  let count = 0;
  for (const line of contents.split(/\r?\n/)) {
    const parsed = parseSshEd25519Line(line);
    if (!parsed) continue;
    const fp = await fingerprintOf(parsed.pubkey);
    await db
      .insertInto("authorized_keys")
      .values({ fingerprint: fp, public_key: parsed.pubkey, label: parsed.label, added_at: now })
      .onConflict((oc) => oc.column("fingerprint").doUpdateSet({ label: parsed.label }))
      .execute();
    count++;
  }
  return count;
}
