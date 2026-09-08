// Decides whether `roost deploy <host>` / `roost keeper-refresh <host>` names
// this box. deploy.ts, direct-keeper-update.ts and keeper-refresh.ts route on
// the answer, choosing the local driver over ssh. Host identity only — it uses
// @roost/shared/tailnet's single MagicDNS reader and nothing else.

import * as os from "node:os";
import { resolveTailnetDnsName } from "@roost/shared/tailnet";

/** True when `host` resolves to the box we're running on. Three signals:
 *  (1) literal localhost/127.0.0.1/::1
 *  (2) case-insensitive match against `os.hostname()` (with .local strip)
 *  (3) match against this host's MagicDNS name.
 *  The (3) path is load-bearing: a Mac's system hostname and its MagicDNS
 *  hostname routinely diverge (system hostname `<host>-air-old` vs MagicDNS
 *  identity `<host>.<tailnet>.ts.net`). Without (3), `deploy <host>`
 *  falls through to remote ssh-to-self and fails permission-denied
 *  because the box doesn't trust its own pubkey by default. */
export async function _isSelfHost(host: string): Promise<boolean> {
  const lower = host.toLowerCase();
  if (lower === "localhost" || lower === "127.0.0.1" || lower === "::1") return true;
  const hostBase = lower.replace(/\.local$|\.$/g, "");
  try {
    const meBase = os.hostname().toLowerCase().replace(/\.local$/, "");
    if (meBase === hostBase) return true;
  } catch { /* ignore */ }
  const dns = resolveTailnetDnsName();
  // MagicDNS name = "<host>.<tailnet>.ts.net" — match the full FQDN OR the
  // leading label (e.g. host = "<host>").
  return dns !== "" && (dns === hostBase || dns.split(".")[0] === hostBase);
}
