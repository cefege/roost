// Pins canonical version-1 pairing ceremony values at the shared boundary.
// Browser and coordinator callers rely on the same strict value shapes.
// Entropy stays opaque; tests assert only its externally visible representation.

import { expect, test } from "bun:test";
import {
  PAIRING_CEREMONY_VERSION,
  PAIR_VERIFICATION_CODE_LENGTH,
  generatePairRequestId,
  generatePairRequesterToken,
  generatePairVerificationCode,
  normalizePairRequestId,
  normalizePairRequesterToken,
  normalizePairVerificationCode,
} from "../src/pairing.ts";

const REQUEST_ID = "0123456789abcdef0123456789abcdef";
const REQUESTER_TOKEN = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

test("generates canonical version-1 pairing values", () => {
  expect(PAIRING_CEREMONY_VERSION).toBe(1);
  expect(PAIR_VERIFICATION_CODE_LENGTH).toBe(6);
  expect(generatePairRequestId()).toMatch(/^[0-9a-f]{32}$/);
  expect(generatePairRequesterToken()).toMatch(/^[0-9a-f]{64}$/);
  for (let sample = 0; sample < 100; sample += 1) {
    expect(generatePairVerificationCode()).toMatch(/^[0-9]{6}$/);
  }
});

test("accepts only canonical pairing request IDs, tokens, and codes", () => {
  expect(normalizePairRequestId(REQUEST_ID)).toBe(REQUEST_ID);
  expect(normalizePairRequesterToken(REQUESTER_TOKEN)).toBe(REQUESTER_TOKEN);
  expect(normalizePairVerificationCode("000042")).toBe("000042");

  for (const malformedRequestId of [
    REQUEST_ID.toUpperCase(),
    `${REQUEST_ID}0`,
    REQUEST_ID.slice(1),
    REQUEST_ID.replace("a", "g"),
    ` ${REQUEST_ID}`,
  ]) {
    expect(normalizePairRequestId(malformedRequestId)).toBeNull();
  }
  for (const malformedRequesterToken of [
    REQUESTER_TOKEN.toUpperCase(),
    `${REQUESTER_TOKEN}0`,
    REQUESTER_TOKEN.slice(1),
    REQUESTER_TOKEN.replace("a", "g"),
    `${REQUESTER_TOKEN} `,
  ]) {
    expect(normalizePairRequesterToken(malformedRequesterToken)).toBeNull();
  }
  for (const malformedCode of ["12345", "1234567", "12 456", "１２３４５６", "12a456"]) {
    expect(normalizePairVerificationCode(malformedCode)).toBeNull();
  }
  expect(normalizePairVerificationCode(123456 as unknown as string)).toBeNull();
});
