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
import { diag } from "@roost/shared/diag";

export interface FragmentDispatcherDependencies {
  peek(): CapturedFragmentCredential | null;
  clear(expectedKind: CapturedFragmentCredentialKind): boolean;
  reload(): void;
  redeemPair(token: string): Promise<RedeemResult>;
  warn(message: string): void;
}

export async function dispatchCapturedFragmentCredential(
  deps: FragmentDispatcherDependencies,
): Promise<boolean> {
  const credential = deps.peek();
  if (credential?.kind !== "pair") return false;

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
    warn: (message) => diag("pair.redeem_failed", { msg: message }),
  });
}


