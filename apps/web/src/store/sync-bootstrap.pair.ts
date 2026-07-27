// Pair-token redeem (FQDN auth path). Split out of sync-bootstrap.ts
// (400-line cap); sync-bootstrap.ts imports _attemptPairRedeem back. No
// import the other way.
//
// The old 5 s pairList poller is GONE (perf sweep C2.4): pending pair
// requests now ride the Sync firehose — coord seeds a full snapshot per
// connect and publishes pending/removed deltas (see sync.ts
// "pairRequestDelta" + coord handlers-auth/handlers-streaming).
import { redeemCoordinatorRelocation } from "../auth/coordinator-relocation.ts";


/** Redeem a ?pair=<token> query param on load. This is the FQDN-reachable
 *  auth path (the loopback-only self-register can't run over the tailnet):
 *  the installer-opened host browser and the QR-scanning phone both arrive
 *  at https://<fqdn>:4102/?pair=<token>. On success we strip the token from
 *  the URL and reload so _bootstrap re-runs authed. Returns true when it
 *  redeemed + triggered a reload (caller must halt). */
export async function _attemptPairRedeem(): Promise<boolean> {
  if (await redeemCoordinatorRelocation()) return true;
  if (typeof location === "undefined") return false;
  const params = new URLSearchParams(location.search);
  const token = params.get("pair");
  if (!token) return false;

  // Lazy import: the redeem path is hit at most once per load, so the auth-crypto
  // module stays out of the store's boot graph until a ?pair token is present.
  const { redeemPairToken } = await import("../auth/redeemPairToken.ts");
  const res = await redeemPairToken(token);

  // Strip ?pair= regardless of outcome — never leave a (now-used) token in
  // the URL or back/forward history.
  params.delete("pair");
  const clean = location.pathname + (params.toString() ? `?${params.toString()}` : "") + location.hash;
  history.replaceState(null, "", clean);

  if (!res.ok) {
    console.warn("[sync] ?pair redeem failed:", res.error);
    return false;
  }
  location.reload();
  return true;
}


