// Hex SHA-256 of a raw 32-byte ed25519 public key — the fingerprint that
// identifies a worker, coordinator, or browser device on the wire. The harness
// derives it for the API key it authorizes, so a byte-for-byte divergence from
// the coordinator's derivation breaks JWT `kid` lookup and authorized-keys
// matching.

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
