// Requester pairing ceremony state is tab-scoped and never enters rootStore or
// cross-tab storage. This owner validates the persisted capability before the
// requester controller retries its exact create or token-bound poll.

import {
  PAIRING_CEREMONY_VERSION,
  normalizePairRequestId,
  normalizePairRequesterToken,
} from "@roost/protocol/pairing";

export const PAIRING_CEREMONY_STORAGE_KEY = "roost.pairingCeremony.v1";

export interface PairingCeremony {
  ceremonyVersion: typeof PAIRING_CEREMONY_VERSION;
  ephemeralId: string;
  requesterToken: string;
}

function pairingSessionStorage(): Storage | null {
  try {
    return typeof sessionStorage === "undefined" ? null : sessionStorage;
  } catch {
    return null;
  }
}

function parsePairingCeremony(raw: string | null): PairingCeremony | null {
  if (raw === null) return null;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return null;
    const value = parsed as Record<string, unknown>;
    const keys = Object.keys(value);
    if (
      keys.length !== 3
      || !keys.includes("ceremonyVersion")
      || !keys.includes("ephemeralId")
      || !keys.includes("requesterToken")
    ) return null;
    if (value.ceremonyVersion !== PAIRING_CEREMONY_VERSION) return null;
    if (typeof value.ephemeralId !== "string" || typeof value.requesterToken !== "string") {
      return null;
    }
    const ephemeralId = normalizePairRequestId(value.ephemeralId);
    const requesterToken = normalizePairRequesterToken(value.requesterToken);
    if (ephemeralId === null || requesterToken === null) return null;
    return { ceremonyVersion: PAIRING_CEREMONY_VERSION, ephemeralId, requesterToken };
  } catch {
    return null;
  }
}

export function loadPairingCeremony(): PairingCeremony | null {
  const storage = pairingSessionStorage();
  if (storage === null) return null;
  try {
    const raw = storage.getItem(PAIRING_CEREMONY_STORAGE_KEY);
    const ceremony = parsePairingCeremony(raw);
    if (raw !== null && ceremony === null) storage.removeItem(PAIRING_CEREMONY_STORAGE_KEY);
    return ceremony;
  } catch {
    return null;
  }
}

export function savePairingCeremony(ceremony: PairingCeremony): void {
  try {
    pairingSessionStorage()?.setItem(
      PAIRING_CEREMONY_STORAGE_KEY,
      JSON.stringify(ceremony),
    );
  } catch {
    // The current document still owns its in-memory requester capability.
  }
}

export function clearPairingCeremony(): void {
  try {
    pairingSessionStorage()?.removeItem(PAIRING_CEREMONY_STORAGE_KEY);
  } catch {
    // The current document clears its in-memory requester capability separately.
  }
}

export function compactPairVerificationCode(value: string): string {
  return value.replace(/\s/g, "");
}
