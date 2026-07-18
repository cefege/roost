// Reverse-resolve a tailnet IP → hostname by parsing `tailscale status --json`.
// Used by the viewer-presence tracker in connect/router.ts to augment the
// per-browser label with the actual machine name (e.g. "Chrome on
// <tailnet-host>"). Refresh every 60s. Graceful no-op if the tailscale
// binary is missing or tailscaled is down: resolveHostname returns null
// and callers fall back to authorized_keys.label.

import { log } from "@roost/shared";
import { existsSync } from "node:fs";

const REFRESH_INTERVAL_MS = 60_000;
const STATUS_TIMEOUT_MS = 2_000;
// Prefer the standalone CLI: in the coord's LaunchAgent context the GUI app
// binary can't reach the daemon and prints a non-JSON error ("The Tailscale
// ..."), whereas the Homebrew CLI connects to the same tailscaled and returns
// clean JSON. Fall back to the GUI app binary if the CLI isn't installed.
const TAILSCALE_BIN = [
  "/opt/homebrew/bin/tailscale",
  "/usr/local/bin/tailscale",
  "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
].find((p) => existsSync(p)) ?? "/opt/homebrew/bin/tailscale";

interface TailscalePeer {
  HostName?: string;
  DNSName?: string;
  TailscaleIPs?: string[];
}
interface TailscaleStatus {
  Self?: TailscalePeer;
  Peer?: Record<string, TailscalePeer>;
}

let _ipToHost: Map<string, string> = new Map();
let _started = false;
let _binaryMissing = false;

export function startTailnetResolver(): void {
  if (_started) return;
  _started = true;
  void _refresh();
  setInterval(() => void _refresh(), REFRESH_INTERVAL_MS).unref?.();
}

/** Strip optional `[]` brackets and `:port` suffix, then look up in the
 *  cached IP→hostname map. Returns null when the IP isn't a tailnet
 *  peer (LAN client, loopback, resolver hasn't run yet, binary missing). */
export function resolveHostname(rawAddr: string | null | undefined): string | null {
  if (!rawAddr) return null;
  const ip = _stripPort(rawAddr);
  return _ipToHost.get(ip) ?? null;
}

function _stripPort(addr: string): string {
  // IPv6 with brackets: [fd7a:...]:1234
  if (addr.startsWith("[")) {
    const end = addr.indexOf("]");
    return end > 0 ? addr.slice(1, end) : addr;
  }
  // IPv4 with port: 100.x.y.z:1234 — count colons to distinguish bare IPv6
  const colon = addr.lastIndexOf(":");
  if (colon < 0) return addr;
  if (addr.indexOf(":") !== colon) return addr; // bare IPv6, no port
  return addr.slice(0, colon);
}

async function _refresh(): Promise<void> {
  if (_binaryMissing) return;
  try {
    const proc = Bun.spawn([TAILSCALE_BIN, "status", "--json"], {
      stdout: "pipe", stderr: "ignore",
    });
    const text = await Promise.race([
      new Response(proc.stdout).text(),
      new Promise<string>((_, reject) =>
        setTimeout(() => { proc.kill(); reject(new Error("timeout")); }, STATUS_TIMEOUT_MS),
      ),
    ]);
    // tailscale prepends non-JSON lines to stdout when the client/tailscaled
    // versions differ ("Warning: client version ... != tailscaled server").
    // Parse from the first "{" so the warning doesn't break resolution.
    const jsonStart = text.indexOf("{");
    if (jsonStart < 0) {
      // No JSON at all on stdout — tailscaled is transiently unreachable
      // (e.g. during a network blip the CLI prints "The Tailscale daemon is
      // not running." with no body). Keep the prior IP→host map and skip
      // quietly; this recovers on the next 60s tick. Previously this fell
      // through to JSON.parse("The …") → a warn spammed every minute.
      return;
    }
    const parsed = JSON.parse(text.slice(jsonStart)) as TailscaleStatus;
    const next = new Map<string, string>();
    _addPeer(next, parsed.Self);
    if (parsed.Peer) for (const peer of Object.values(parsed.Peer)) _addPeer(next, peer);
    _ipToHost = next;
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    if (msg.includes("ENOENT") || msg.includes("not found")) {
      _binaryMissing = true;
      log.warn("tailnet-resolver", "binary_missing_disabling", { bin: TAILSCALE_BIN });
      return;
    }
    log.warn("tailnet-resolver", "refresh_failed", { err: msg });
  }
}

function _addPeer(map: Map<string, string>, peer: TailscalePeer | undefined): void {
  if (!peer || !peer.HostName) return;
  for (const ip of peer.TailscaleIPs ?? []) map.set(ip, peer.HostName);
}
