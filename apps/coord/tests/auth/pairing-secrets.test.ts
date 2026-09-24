// Pairing coordinator adapters must share browser ceremony validation exactly.
// This suite leaves entropy coverage with @roost/protocol and pins digest-only
// coordinator ownership plus strict pre-hash wire values.

import { describe, expect, test } from "bun:test";
import {
  PAIRING_CEREMONY_VERSION,
  generatePairRequestId,
  generatePairRequesterToken,
  generatePairVerificationCode,
  normalizePairRequestId,
  normalizePairRequesterToken,
  normalizePairVerificationCode,
} from "@roost/protocol/pairing";
import {
  assertPairingCeremonyVersion,
  pairingSecretDigest,
} from "../../src/auth/pairing-secrets.ts";

describe("pairing coordinator adapters", () => {
  test("uses shared fixed-width lowercase requester values and six-digit codes", () => {
    expect(PAIRING_CEREMONY_VERSION).toBe(1);
    expect(generatePairRequestId()).toMatch(/^[0-9a-f]{32}$/);
    expect(generatePairRequesterToken()).toMatch(/^[0-9a-f]{64}$/);
    for (let sample = 0; sample < 100; sample += 1) {
      expect(generatePairVerificationCode()).toMatch(/^[0-9]{6}$/);
    }
  });

  test("admits only canonical shared wire values", () => {
    expect(normalizePairRequestId("ab".repeat(16))).toBe("ab".repeat(16));
    expect(normalizePairRequesterToken("cd".repeat(32))).toBe("cd".repeat(32));
    expect(normalizePairVerificationCode("000042")).toBe("000042");
    for (const malformed of [
      "ab".repeat(15),
      "AB".repeat(16),
      "cd".repeat(31),
      "CD".repeat(32),
      "12345",
      "１２３４５６",
      "12345a",
    ]) {
      expect(
        normalizePairRequestId(malformed)
        ?? normalizePairRequesterToken(malformed)
        ?? normalizePairVerificationCode(malformed),
      ).toBeNull();
    }
  });

  test("refuses every stale ceremony version with the reload instruction", () => {
    expect(() => assertPairingCeremonyVersion(0)).toThrow("pairing client must reload");
    expect(() => assertPairingCeremonyVersion(2)).toThrow("pairing client must reload");
    expect(() => assertPairingCeremonyVersion(PAIRING_CEREMONY_VERSION)).not.toThrow();
  });

  test("digests secrets without reproducing plaintext", async () => {
    const plaintext = "012345";
    const digest = await pairingSecretDigest(plaintext);
    expect(digest).toMatch(/^[0-9a-f]{64}$/);
    expect(digest).not.toContain(plaintext);
    expect(await pairingSecretDigest(plaintext)).toBe(digest);
  });
});
