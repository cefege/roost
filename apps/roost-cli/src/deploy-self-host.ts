import * as os from "node:os";
import { run, DeployFailure } from "./deploy-exec.ts";

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
      let j: { Self?: { DNSName?: string; HostName?: string } };
      try {
        j = JSON.parse(ts.stdout);
      } catch {
        // Exit 0 with non-JSON payload is NOT the same failure as
        // "tailscale absent": swallowing it here would misroute a self
        // deploy into ssh-to-self and die on a misleading permission error.
        throw new DeployFailure(
          3,
          `tailscale status --json returned malformed output: ${ts.stdout.trim().slice(0, 200)}`,
        );
      }
      const dns = (j.Self?.DNSName ?? "").toLowerCase().replace(/\.$/, "");
      // DNSName = "<host>.<tailnet>.ts.net" — match the full
      // FQDN OR the leading label (e.g. host = "<host>").
      const dnsLabel = dns.split(".")[0];
      if (dns && (dns === hostBase || dnsLabel === hostBase)) return true;
    }
  } catch (error) {
    if (error instanceof DeployFailure) throw error;
    /* ignore — tailscale not installed / not running */
  }
  return false;
}
