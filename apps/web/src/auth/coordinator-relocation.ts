import { createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import { coordClient } from "../connect.ts";
import { getPublicKeyB64 } from "./web-key.ts";
import { browserSelfLabel } from "../lib/browserSelfLabel.ts";

const MOVE_FRAGMENT_KEY = "move";
const HANDOFF_FRAGMENT_KEY = "handoff";
let relocating = false;

export interface CoordinatorRelocationFragment {
  token: string;
  handoffId: string;
}

export interface RetiredCoordinatorIdentity {
  relocatedToUrl?: string;
  handoffId?: string;
}


/** Redeem a one-time relocation token already parsed and scrubbed by startup. */
export async function redeemCoordinatorRelocation(
  relocation: CoordinatorRelocationFragment,
): Promise<boolean> {
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
    location.reload();
    return true;
  } catch (error) {
    console.error("[coord-relocation] destination redemption failed", error);
    return false;
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
  try {
    const minted = await coordClient.authMintCoordinatorRelocation({ handoffId });
    const target = new URL(`${location.pathname}${location.search}`, minted.targetUrl || targetUrl);
    target.hash = new URLSearchParams({
      [MOVE_FRAGMENT_KEY]: minted.token,
      [HANDOFF_FRAGMENT_KEY]: handoffId,
    }).toString();
    location.assign(target.href);
    return "started";
  } catch (error) {
    relocating = false;
    console.error("[coord-relocation] source token mint failed", error);
    return "failed";
  }
}

/** Follow a retired coordinator through its authenticated relocation mint. */
export async function relocateRetiredBrowser(identity: RetiredCoordinatorIdentity): Promise<RelocationOutcome> {
  if (!identity.relocatedToUrl || !identity.handoffId) return "failed";
  return relocateBrowserToCoordinator(identity.handoffId, identity.relocatedToUrl);
}
