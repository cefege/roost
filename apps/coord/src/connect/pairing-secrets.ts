// Coordinator pairing protocol adapters validate the shared ceremony contract
// before handlers hash or query. This module owns only coordinator-specific
// digesting and the confirmation attempt bound; browser-safe entropy lives shared.

import { Code, ConnectError } from "@connectrpc/connect";
import {
  PAIRING_CEREMONY_VERSION,
  PAIR_VERIFICATION_CODE_LENGTH,
  normalizePairRequestId,
  normalizePairRequesterToken,
  normalizePairVerificationCode,
} from "@roost/shared/pairing";
import { bootstrapTokenDigest } from "../bootstrap-tokens.ts";

export {
  PAIRING_CEREMONY_VERSION,
  PAIR_VERIFICATION_CODE_LENGTH,
  normalizePairRequestId,
  normalizePairRequesterToken,
  normalizePairVerificationCode,
};

export const PAIR_VERIFICATION_ATTEMPT_LIMIT = 5;
export const PAIRING_CLIENT_RELOAD_MESSAGE = "pairing client must reload";

export function assertPairingCeremonyVersion(ceremonyVersion: number): void {
  if (ceremonyVersion !== PAIRING_CEREMONY_VERSION) {
    throw new ConnectError(PAIRING_CLIENT_RELOAD_MESSAGE, Code.FailedPrecondition);
  }
}

export async function pairingSecretDigest(plaintext: string): Promise<string> {
  return bootstrapTokenDigest(plaintext);
}
