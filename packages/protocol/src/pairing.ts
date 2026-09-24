// Versioned pairing ceremony entropy and canonical wire-value validation.
// Browser requesters and approvers create values here; the coordinator only hashes them.
// This module stays browser-safe and never owns stored secrets or digests.

export const PAIRING_CEREMONY_VERSION = 1;
export const PAIR_VERIFICATION_CODE_LENGTH = 6;

const PAIR_REQUEST_ID_BYTES = 16;
const PAIR_REQUESTER_TOKEN_BYTES = 32;
const PAIR_VERIFICATION_CODE_SPACE = 10 ** PAIR_VERIFICATION_CODE_LENGTH;
const UINT32_SPACE = 0x1_0000_0000;
const UNBIASED_VERIFICATION_CODE_LIMIT = Math.floor(
  UINT32_SPACE / PAIR_VERIFICATION_CODE_SPACE,
) * PAIR_VERIFICATION_CODE_SPACE;
const LOWERCASE_HEX_PATTERN = /^[0-9a-f]+$/;
const ASCII_DIGITS_PATTERN = /^[0-9]+$/;

export function generatePairRequestId(): string {
  return generateLowercaseHex(PAIR_REQUEST_ID_BYTES);
}

export function generatePairRequesterToken(): string {
  return generateLowercaseHex(PAIR_REQUESTER_TOKEN_BYTES);
}

export function generatePairVerificationCode(): string {
  const random = new Uint32Array(1);
  do {
    crypto.getRandomValues(random);
  } while (random[0]! >= UNBIASED_VERIFICATION_CODE_LIMIT);
  return String(random[0]! % PAIR_VERIFICATION_CODE_SPACE).padStart(
    PAIR_VERIFICATION_CODE_LENGTH,
    "0",
  );
}

export function normalizePairRequestId(value: string): string | null {
  return typeof value === "string"
    && value.length === PAIR_REQUEST_ID_BYTES * 2
    && LOWERCASE_HEX_PATTERN.test(value)
    ? value
    : null;
}

export function normalizePairRequesterToken(value: string): string | null {
  return typeof value === "string"
    && value.length === PAIR_REQUESTER_TOKEN_BYTES * 2
    && LOWERCASE_HEX_PATTERN.test(value)
    ? value
    : null;
}

export function normalizePairVerificationCode(value: string): string | null {
  return typeof value === "string"
    && value.length === PAIR_VERIFICATION_CODE_LENGTH
    && ASCII_DIGITS_PATTERN.test(value)
    ? value
    : null;
}

function generateLowercaseHex(byteLength: number): string {
  const random = new Uint8Array(byteLength);
  crypto.getRandomValues(random);
  return Array.from(random, (byte) => byte.toString(16).padStart(2, "0")).join("");
}
