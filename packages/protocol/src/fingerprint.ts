// Hex SHA-256 of a raw 32-byte ed25519 public key — the fingerprint that
// identifies a worker, coordinator, or browser device on the wire. All three
// ends of the protocol derive it independently, so a byte-for-byte divergence
// here silently breaks pairing, JWT `kid` lookup, and authorized-keys matching.
//
// It used to exist three times with nothing tying the copies together:
// coord/src/jwt.ts (crypto.subtle.digest), worker/src/jwt.ts (node:crypto
// createHash), and web/src/auth/web-key.ts (crypto.subtle.digest again). This
// module is the one definition.
//
// crypto.subtle is deliberate over node:crypto createHash: it is the only
// implementation available in every runtime that computes this value — the
// browser included — so the shared definition can serve all three ends instead
// of forcing web to keep a copy.

/** Hex SHA-256 of a raw ed25519 public key. */
export async function fingerprintOf(raw: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    raw.buffer.slice(raw.byteOffset, raw.byteOffset + raw.byteLength) as ArrayBuffer,
  );
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}
