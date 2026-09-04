// This module owns the browser half of a live coordinator handoff.
// Settings and sync mint on the current coordinator; startup redeems after fragment scrubbing.
// It depends on coordinator RPCs, the browser public key, and fragment-only navigation.
// A module latch collapses racing commit notifications into one leak-free destination navigation.

import { Code, ConnectError, createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import { diag } from "@roost/shared/diag";
import { coordClient } from "../connect.ts";
import { getPublicKeyB64 } from "./web-key.ts";
import { browserSelfLabel } from "../lib/browserSelfLabel.ts";
import type { CapturedFragmentCredential } from "./fragment-credential.ts";

const MOVE_FRAGMENT_KEY = "move";
const HANDOFF_FRAGMENT_KEY = "handoff";
let relocating = false;
let relocationGeneration = 0;

export type CoordinatorRelocationFragment = Extract<
  CapturedFragmentCredential,
  { kind: "relocation" }
>;

export interface RetiredCoordinatorIdentity {
  relocatedToUrl?: string;
  handoffId?: string;
}

export type RelocationRedemptionResult =
  | "success"
  | "authoritative-denial"
  | "retryable";

/** Redeem a one-time relocation token already parsed and scrubbed by entry.ts. */
export async function redeemCoordinatorRelocation(
  relocation: CoordinatorRelocationFragment,
): Promise<RelocationRedemptionResult> {
  try {
    const client = createClient(CoordinatorService, createConnectTransport({
      baseUrl: "/",
      useBinaryFormat: true,
    }));
    await client.authRedeemCoordinatorRelocation({
      token: relocation.token,
      sshPubkeyB64: await getPublicKeyB64(),
      label: browserSelfLabel(),
    });
    return "success";
  } catch (error) {
    const authoritative = error instanceof ConnectError && (
      error.code === Code.InvalidArgument
      || error.code === Code.AlreadyExists
      || error.code === Code.PermissionDenied
      || error.code === Code.Unauthenticated
    );
    diag("coord_move.spa_redeem_failed", {
      outcome: authoritative ? "denied" : "retryable",
      error: String(error),
    });
    return authoritative ? "authoritative-denial" : "retryable";
  }
}

export type RelocationOutcome = "started" | "in-flight" | "failed";

/** Mint on the current coordinator, then navigate without leaking a token in requests. */
export async function relocateBrowserToCoordinator(handoffId: string, targetUrl: string): Promise<RelocationOutcome> {
  // Two callers race at COMMITTED (the sync frame and the dialog poll).
  // "already navigating" is a success, not the failure the dialog used to
  // report as "Could not redirect automatically" on a clean move.
  if (relocating) return "in-flight";
  relocating = true;
  const requestGeneration = relocationGeneration;
  try {
    const minted = await coordClient.authMintCoordinatorRelocation({ handoffId });
    if (requestGeneration !== relocationGeneration) return "failed";
    const target = new URL(`${location.pathname}${location.search}`, minted.targetUrl || targetUrl);
    target.hash = new URLSearchParams({
      [MOVE_FRAGMENT_KEY]: minted.token,
      [HANDOFF_FRAGMENT_KEY]: handoffId,
    }).toString();
    location.assign(target.href);
    return "started";
  } catch (error) {
    if (requestGeneration === relocationGeneration) relocating = false;
    diag("coord_move.spa_mint_failed", { handoff_id: handoffId, error: String(error) });
    return "failed";
  }
}

/** Follow a retired coordinator through its authenticated relocation mint. */
export async function relocateRetiredBrowser(identity: RetiredCoordinatorIdentity): Promise<RelocationOutcome> {
  if (!identity.relocatedToUrl || !identity.handoffId) return "failed";
  return relocateBrowserToCoordinator(identity.handoffId, identity.relocatedToUrl);
}

/** Let a later account start a fresh relocation after this one logs out mid-mint. */
export function clearCoordinatorRelocationRuntimeForLogout(): void {
  relocationGeneration += 1;
  relocating = false;
}
