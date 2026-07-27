// Resolve this host's Tailscale MagicDNS name without letting a stalled GUI
// shim block coordinator or worker startup. The caller may provide explicit
// binary paths in tests; production probes common macOS installations.
const TAILSCALE_BINS = [
  "tailscale",
  "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
  "/opt/homebrew/bin/tailscale",
  "/usr/local/bin/tailscale",
] as const;

export function resolveTailnetDnsName(bins: readonly string[] = TAILSCALE_BINS): string {
  for (const bin of bins) {
    try {
      const result = Bun.spawnSync([bin, "status", "--json"], {
        timeout: 2_000,
        killSignal: "SIGKILL",
      });
      if (result.exitCode !== 0) continue;
      const status = JSON.parse(result.stdout.toString()) as { Self?: { DNSName?: string } };
      const dnsName = (status.Self?.DNSName ?? "").toLowerCase().replace(/\.$/, "");
      if (dnsName) return dnsName;
    } catch {
      // Try the next known installation path.
    }
  }
  return "";
}
