// Redeem a browser pairing token → authorize THIS browser with coord.
// Shared by two callers:
//   - Onboarding.tsx paste flow ("I have a pairing code")
//   - startup #pair=<token> fragment dispatcher (installer-opened browser and
//     QR-scanning phone); the dispatcher scrubs the bearer before this runs.
// On success the caller reloads so bootstrapSync re-runs with the authed JWT.

import { coordClient } from "../connect.ts";
import { getPublicKeyB64 } from "./web-key.ts";
import { trustCoordFingerprint } from "./trust.ts";
import { browserSelfLabel } from "../lib/browserSelfLabel.ts";

export type RedeemResult = { ok: true } | { ok: false; error: string };

export async function redeemPairToken(token: string): Promise<RedeemResult> {
  try {
    const pubkeyB64 = await getPublicKeyB64();
    const result = await coordClient.authRedeemBrowser({
      token,
      sshPubkeyB64: pubkeyB64,
      label: browserSelfLabel(),
    });
    if (!result.fingerprint) return { ok: false, error: "no fingerprint in response" };
    await trustCoordFingerprint(result.fingerprint);
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}
