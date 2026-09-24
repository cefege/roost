// Approver pairing state is tab-scoped and never enters rootStore, Sync, URLs,
// or cross-tab storage. The provider persists exactly one generated code so an
// ambiguous approval can retry idempotently after a reload.

import {
  PAIRING_CEREMONY_VERSION,
  normalizePairRequestId,
  normalizePairVerificationCode,
} from "@roost/protocol/pairing";

export const PAIR_APPROVAL_STORAGE_KEY = "roost.pairApproval.v1";

export interface PairApprovalRecord {
  ceremonyVersion: typeof PAIRING_CEREMONY_VERSION;
  ephemeralId: string;
  verificationCode: string;
  requesterLabel: string;
  expiresAtMs: number;
}

function approvalSessionStorage(): Storage | null {
  try {
    return typeof sessionStorage === "undefined" ? null : sessionStorage;
  } catch {
    return null;
  }
}

function parsePairApproval(raw: string | null): PairApprovalRecord | null {
  if (raw === null) return null;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return null;
    const value = parsed as Record<string, unknown>;
    const keys = Object.keys(value);
    if (
      keys.length !== 5
      || !keys.includes("ceremonyVersion")
      || !keys.includes("ephemeralId")
      || !keys.includes("verificationCode")
      || !keys.includes("requesterLabel")
      || !keys.includes("expiresAtMs")
    ) return null;
    if (
      value.ceremonyVersion !== PAIRING_CEREMONY_VERSION
      || typeof value.ephemeralId !== "string"
      || typeof value.verificationCode !== "string"
      || typeof value.requesterLabel !== "string"
      || typeof value.expiresAtMs !== "number"
      || !Number.isSafeInteger(value.expiresAtMs)
      || value.expiresAtMs <= 0
    ) return null;
    const ephemeralId = normalizePairRequestId(value.ephemeralId);
    const verificationCode = normalizePairVerificationCode(value.verificationCode);
    if (ephemeralId === null || verificationCode === null) return null;
    return {
      ceremonyVersion: PAIRING_CEREMONY_VERSION,
      ephemeralId,
      verificationCode,
      requesterLabel: value.requesterLabel,
      expiresAtMs: value.expiresAtMs,
    };
  } catch {
    return null;
  }
}

export function loadPairApproval(): PairApprovalRecord | null {
  const storage = approvalSessionStorage();
  if (storage === null) return null;
  try {
    const raw = storage.getItem(PAIR_APPROVAL_STORAGE_KEY);
    const record = parsePairApproval(raw);
    if (raw !== null && record === null) storage.removeItem(PAIR_APPROVAL_STORAGE_KEY);
    return record;
  } catch {
    return null;
  }
}

export function savePairApproval(record: PairApprovalRecord): void {
  try {
    approvalSessionStorage()?.setItem(PAIR_APPROVAL_STORAGE_KEY, JSON.stringify(record));
  } catch {
    // The current document retains the generated code until it is dismissed.
  }
}

export function clearPairApproval(): void {
  try {
    approvalSessionStorage()?.removeItem(PAIR_APPROVAL_STORAGE_KEY);
  } catch {
    // The in-memory provider state is cleared independently.
  }
}
