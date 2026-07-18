import * as os from "node:os";
import { run } from "./deploy-exec.ts";

/** True when `host` resolves to the box we're running on. Three signals:
 *  (1) literal localhost/127.0.0.1/::1
 *  (2) case-insensitive match against `os.hostname()` (with .local strip)
 *  (3) match against the tailnet Self.DNSName from `tailscale status --json`.
 *  The (3) path is load-bearing: a Mac's system hostname and its tailnet
 *  hostname routinely diverge (system hostname `<host>-air-old` vs tailnet
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
  try {
    const ts = await run(["tailscale", "status", "--json"], { quiet: true });
    if (ts.exit === 0) {
      const j = JSON.parse(ts.stdout) as { Self?: { DNSName?: string; HostName?: string } };
      const dns = (j.Self?.DNSName ?? "").toLowerCase().replace(/\.$/, "");
      // DNSName = "<host>.<tailnet>.ts.net" — match the full
      // FQDN OR the leading label (e.g. host = "<host>").
      const dnsLabel = dns.split(".")[0];
      if (dns && (dns === hostBase || dnsLabel === hostBase)) return true;
    }
  } catch { /* ignore — tailscale not installed / not running */ }
  return false;
}
