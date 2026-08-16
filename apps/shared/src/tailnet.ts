// Resolve this host's Tailscale MagicDNS name from structured JSON without
// depending on a service manager's often-minimal PATH.
import { win32 } from "node:path";
import { assertNeverPlatform, supportedHostPlatform, type SupportedHostPlatform } from "./platform.ts";

type Env = Record<string, string | undefined>;

export function tailscaleBinaryCandidates(
  platform: SupportedHostPlatform = supportedHostPlatform(),
  env: Env = process.env,
): string[] {
  const explicit = env.ROOST_TAILSCALE_BIN ? [env.ROOST_TAILSCALE_BIN] : [];
  switch (platform) {
    case "darwin":
      return [...explicit, "tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/opt/homebrew/bin/tailscale", "/usr/local/bin/tailscale"];
    case "linux":
      return [...explicit, "tailscale", "/usr/bin/tailscale", "/usr/local/bin/tailscale"];
    case "win32": {
      const roots = [env.ProgramW6432, env.ProgramFiles, env.LOCALAPPDATA].filter(
        (value): value is string => Boolean(value),
      );
      const installed = roots.map((root) => win32.join(root, "Tailscale", "tailscale.exe"));
      return [...explicit, "tailscale.exe", ...new Set(installed)];
    }
    default:
      return assertNeverPlatform(platform);
  }
}

export function resolveTailnetDnsName(
  bins: readonly string[] = tailscaleBinaryCandidates(),
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  for (const bin of bins) {
    try {
      const result = Bun.spawnSync([bin, "status", "--json"], {
        timeout: 2_000,
        ...(platform === "win32" ? {} : { killSignal: "SIGKILL" as const }),
      });
      if (result.exitCode !== 0) continue;
      const status = JSON.parse(result.stdout.toString()) as { Self?: { DNSName?: string } };
      const dnsName = (status.Self?.DNSName ?? "").toLowerCase().replace(/\.$/, "");
      if (dnsName) return dnsName;
    } catch {
      // Try the next documented installation path.
    }
  }
  return "";
}
