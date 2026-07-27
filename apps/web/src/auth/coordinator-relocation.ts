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

export function coordinatorRelocationFragment(hash?: string): CoordinatorRelocationFragment | null {
  const fragment = hash ?? (typeof location === "undefined" ? "" : location.hash);
  const params = new URLSearchParams(fragment.slice(1));
  const token = params.get(MOVE_FRAGMENT_KEY);
  const handoffId = params.get(HANDOFF_FRAGMENT_KEY);
  return token && handoffId ? { token, handoffId } : null;
}

/** Redeem a one-time relocation token before any ordinary coordinator call. */
export async function redeemCoordinatorRelocation(): Promise<boolean> {
  const relocation = coordinatorRelocationFragment();
  if (!relocation) return false;
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
    history.replaceState(null, "", `${location.pathname}${location.search}`);
    location.reload();
    return true;
  } catch (error) {
    console.error("[coord-relocation] destination redemption failed", error);
    return false;
  }
}

/** Mint on the current coordinator, then navigate without leaking a token in requests. */
export async function relocateBrowserToCoordinator(handoffId: string, targetUrl: string): Promise<boolean> {
  if (relocating) return false;
  relocating = true;
  try {
    const minted = await coordClient.authMintCoordinatorRelocation({ handoffId });
    const target = new URL(`${location.pathname}${location.search}`, minted.targetUrl || targetUrl);
    target.hash = new URLSearchParams({
      [MOVE_FRAGMENT_KEY]: minted.token,
      [HANDOFF_FRAGMENT_KEY]: handoffId,
    }).toString();
    location.assign(target.href);
    return true;
  } catch (error) {
    relocating = false;
    console.error("[coord-relocation] source token mint failed", error);
    return false;
  }
}

/** Follow a retired coordinator through its authenticated relocation mint. */
export async function relocateRetiredBrowser(identity: RetiredCoordinatorIdentity): Promise<boolean> {
  if (!identity.relocatedToUrl || !identity.handoffId) return false;
  return relocateBrowserToCoordinator(identity.handoffId, identity.relocatedToUrl);
}
