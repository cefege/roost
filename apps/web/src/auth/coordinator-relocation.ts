import { createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import { coordClient } from "../connect.ts";
import { getPublicKeyB64 } from "./web-key.ts";
import { browserSelfLabel } from "../lib/browserSelfLabel.ts";

const FRAGMENT_KEY = "roost-relocation";
let relocating = false;

function relocationToken(): string | null {
  const params = new URLSearchParams(location.hash.slice(1));
  return params.get(FRAGMENT_KEY);
}

/** Redeem a one-time relocation token before the destination app starts. */
export async function redeemCoordinatorRelocation(): Promise<boolean> {
  const token = relocationToken();
  if (!token) return false;
  try {
    const client = createClient(CoordinatorService, createConnectTransport({
      baseUrl: "/",
      useBinaryFormat: true,
    }));
    await client.authRedeemCoordinatorRelocation({
      token,
      sshPubkeyB64: await getPublicKeyB64(),
      label: browserSelfLabel(),
    });
    history.replaceState(null, "", `${location.pathname}${location.search}`);
    return true;
  } catch (error) {
    console.error("[coord-relocation] destination redemption failed", error);
    return false;
  }
}

/** Mint on the old coordinator, then navigate to the target without leaking the token in requests. */
export async function relocateBrowserToCoordinator(handoffId: string, targetUrl: string): Promise<void> {
  if (relocating) return;
  relocating = true;
  try {
    const minted = await coordClient.authMintCoordinatorRelocation({ handoffId });
    const target = new URL(`${location.pathname}${location.search}`, minted.targetUrl || targetUrl);
    target.hash = new URLSearchParams({ [FRAGMENT_KEY]: minted.token }).toString();
    location.assign(target.href);
  } catch (error) {
    relocating = false;
    console.error("[coord-relocation] source token mint failed", error);
  }
}
