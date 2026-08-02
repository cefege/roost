// One startup dispatcher owns every fragment bearer. It scrubs credentials
// before network I/O, then permits exactly one complete credential shape.
import {
  credentialFreeUrl,
  parseFragmentCredential,
  type FragmentCredential,
} from "../auth/fragment-credential.ts";
import type { RedeemResult } from "../auth/redeemPairToken.ts";

export interface FragmentDispatcherDependencies {
  pathname: string;
  search: string;
  hash: string;
  replace(url: string): void;
  reload(): void;
  redeemPair(token: string): Promise<RedeemResult>;
  redeemRelocation(credential: Extract<FragmentCredential, { kind: "relocation" }>): Promise<boolean>;
  warn(message: string): void;
}

export async function dispatchFragmentCredential(
  deps: FragmentDispatcherDependencies,
): Promise<boolean> {
  const credential = parseFragmentCredential(deps.hash);
  if (credential.kind === "none") return false;
  deps.replace(credentialFreeUrl(deps.pathname, deps.search, deps.hash));
  if (credential.kind === "invalid") {
    throw new Error(
      "Invalid credential link (retryable): combined, partial, empty, or duplicate fields",
    );
  }
  if (credential.kind === "relocation") {
    return deps.redeemRelocation(credential);
  }
  const result = await deps.redeemPair(credential.token);
  if (!result.ok) {
    deps.warn(`[sync] #pair redeem failed: ${result.error}`);
    return false;
  }
  deps.reload();
  return true;
}

/** Scrub, classify, and redeem one fragment credential before ordinary RPCs. */
export async function _dispatchFragmentCredential(): Promise<boolean> {
  if (typeof location === "undefined") return false;
  return dispatchFragmentCredential({
    pathname: location.pathname,
    search: location.search,
    hash: location.hash,
    replace: (url) => history.replaceState(null, "", url),
    reload: () => location.reload(),
    redeemPair: async (token) => {
      const { redeemPairToken } = await import("../auth/redeemPairToken.ts");
      return redeemPairToken(token);
    },
    redeemRelocation: async (credential) => {
      const { redeemCoordinatorRelocation } = await import("../auth/coordinator-relocation.ts");
      return redeemCoordinatorRelocation(credential);
    },
    warn: (message) => console.warn(message),
  });
}


