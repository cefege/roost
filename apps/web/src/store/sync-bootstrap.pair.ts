// Startup consumes only entry.ts's already-scrubbed state. Credential state is
// retained across reloads and ambiguous transport failures, then cleared only
// after redemption succeeds or the coordinator authoritatively denies it.
import {
  clearCapturedFragmentCredential,
  peekCapturedFragmentCredential,
} from "../auth/fragment-credential.ts";
import type {
  CapturedFragmentCredential,
  CapturedFragmentCredentialKind,
} from "../auth/fragment-credential.ts";
import { redeemPairToken } from "../auth/redeemPairToken.ts";
import type { RedeemResult } from "../auth/redeemPairToken.ts";
import { redeemCoordinatorRelocation } from "../auth/coordinator-relocation.ts";
import type { RelocationRedemptionResult } from "../auth/coordinator-relocation.ts";

export interface FragmentDispatcherDependencies {
  peek(): CapturedFragmentCredential | null;
  clear(expectedKind: CapturedFragmentCredentialKind): boolean;
  reload(): void;
  redeemPair(token: string): Promise<RedeemResult>;
  redeemRelocation(
    credential: Extract<CapturedFragmentCredential, { kind: "relocation" }>,
  ): Promise<RelocationRedemptionResult>;
  warn(message: string): void;
}

export async function dispatchCapturedFragmentCredential(
  deps: FragmentDispatcherDependencies,
): Promise<boolean> {
  const credential = deps.peek();
  if (credential?.kind !== "pair" && credential?.kind !== "relocation") return false;

  if (credential.kind === "relocation") {
    const result = await deps.redeemRelocation(credential);
    if (result === "retryable") return false;
    deps.clear("relocation");
    if (result === "authoritative-denial") {
      deps.warn("[sync] coordinator relocation credential was denied");
      return false;
    }
    deps.reload();
    return true;
  }

  const result = await deps.redeemPair(credential.token);
  if (!result.ok) {
    if ("authoritative" in result && result.authoritative) deps.clear("pair");
    deps.warn(`[sync] #pair redeem failed: ${result.error}`);
    return false;
  }
  deps.clear("pair");
  deps.reload();
  return true;
}

/** Redeem one scrubbed startup credential before ordinary protected RPCs. */
export async function _dispatchCapturedFragmentCredential(): Promise<boolean> {
  return dispatchCapturedFragmentCredential({
    peek: peekCapturedFragmentCredential,
    clear: clearCapturedFragmentCredential,
    reload: () => location.reload(),
    redeemPair: redeemPairToken,
    redeemRelocation: redeemCoordinatorRelocation,
    warn: (message) => console.warn(message),
  });
}


