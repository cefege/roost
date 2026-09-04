// Native status probes share the bounded subprocess path in this module.
// Both one-shot status and quickstart use the Tailscale checks, while report
// assembly uses the service-manager probe to avoid hanging on native tools.

import { runWindowsHelperSync, type WindowsServiceSnapshot } from "@roost/shared/windows-helper";
import { coordServiceLabel, workerServiceLabel } from "@roost/shared/paths";
import {
  COORD_UNIT,
  WINDOWS_SERVICE_NAMES,
  WORKER_UNIT,
} from "./service-ctl.ts";
import { trustedTailscaleExecutable } from "./windows/windows-identity.ts";

const COORD_LABEL = coordServiceLabel();
const WORKER_LABEL = workerServiceLabel();

// launchctl/systemctl/tailscale probes must never hang the one-shot status
// readout on a wedged service manager; a timed-out probe reports exitCode
// null, which folds into generic non-zero failure below.
const PROBE_DEADLINE_MS = 5_000;

export function captureStatusCommand(cmd: string[]): { exit: number; stdout: string } {
  try {
    const r = Bun.spawnSync(cmd, { timeout: PROBE_DEADLINE_MS });
    return { exit: r.exitCode ?? 1, stdout: r.stdout.toString() };
  } catch {
    return { exit: 127, stdout: "" };
  }
}

/** Tailscale BackendState + own tailnet FQDN. state="NotInstalled" when the
 *  binary is missing. fqdn is the trailing-dot-stripped Self.DNSName.
 *  Exported — quickstart.ts uses it for the hard Tailscale gate. */
export function resolveTailscale(): { state: string; fqdn: string | null } {
  const r = captureStatusCommand([trustedTailscaleExecutable(), "status", "--json"]);
  if (r.exit === 127) return { state: "NotInstalled", fqdn: null };
  if (r.exit !== 0 || !r.stdout) return { state: "Stopped", fqdn: null };
  try {
    const j = JSON.parse(r.stdout) as { BackendState?: string; Self?: { DNSName?: string } };
    const fqdn = (j.Self?.DNSName ?? "").replace(/\.$/, "") || null;
    return { state: j.BackendState ?? "Unknown", fqdn };
  } catch {
    return { state: "Unknown", fqdn: null };
  }
}

export interface TailscalePreflightDeps {
  resolve: () => { state: string; fqdn: string | null };
  log: (msg: string) => void;
  sleep: (ms: number) => Promise<void>;
  now: () => number;
  /** darwin-only; Linux has no scripted install step. */
  brewInstall?: () => void | Promise<void>;
  /** Defaults to the host platform; injected so both guidance branches are testable. */
  platform?: string;
}

/** Interactive Tailscale gate: loop until Tailscale is Running with a tailnet
 *  FQDN, guiding the user through the (sudo, un-scriptable) steps and POLLING
 *  system state — no stdin, so it works under `curl … | bash`. The Homebrew
 *  open-source tailscaled needs NO System Settings network-extension approval
 *  (that's only the GUI App Store/standalone app), so `tailscale cert`/`serve`
 *  are reachable non-interactively. On Linux the distro package plus an
 *  operator grant plays the same role. Guidance prints once; throws on timeout
 *  so the caller dies with the same remedy. Pure/injectable for tests. */
export async function ensureTailscale(
  deps: TailscalePreflightDeps,
  timeoutMs = 180_000,
  pollMs = 1500,
): Promise<{ state: string; fqdn: string }> {
  const deadline = deps.now() + timeoutMs;
  let guided = false;
  for (;;) {
    const ts = deps.resolve();
    if (ts.state === "Running" && ts.fqdn) return { state: ts.state, fqdn: ts.fqdn };
    if (!guided) {
      const darwin = (deps.platform ?? process.platform) === "darwin";
      if (ts.state === "NotInstalled") {
        if (darwin) {
          deps.log("Tailscale is required and not installed. Installing the open-source");
          deps.log("CLI daemon (no System Settings approval needed):");
          deps.log("  brew install tailscale");
          await deps.brewInstall?.();
          deps.log("Then start it (needs sudo — run this yourself):");
          deps.log("  sudo tailscaled install-system-daemon && sudo tailscale up");
        } else {
          deps.log("Tailscale is required and not installed. Install it (needs sudo —");
          deps.log("run this yourself; see https://tailscale.com/download/linux to add the repo):");
          deps.log("  sudo dnf install -y tailscale");
          deps.log("Then start it and grant yourself cert/serve access:");
          deps.log("  sudo systemctl enable --now tailscaled && sudo tailscale up");
          deps.log("  sudo tailscale set --operator=$USER");
        }
      } else if (darwin) {
        deps.log(`Tailscale is installed but not running (state: ${ts.state}). Start it:`);
        deps.log("  sudo tailscaled install-system-daemon && sudo tailscale up");
      } else {
        deps.log(`Tailscale is installed but not running (state: ${ts.state}). Start it:`);
        deps.log("  sudo systemctl enable --now tailscaled && sudo tailscale up");
        deps.log("  sudo tailscale set --operator=$USER");
      }
      deps.log("Waiting for Tailscale to come up… (Ctrl-C to abort)");
      guided = true;
    }
    if (deps.now() >= deadline) {
      throw new Error(
        `Tailscale did not come up within ${Math.round(timeoutMs / 1000)}s. ` +
          "Run the commands above, then re-run: roost quickstart",
      );
    }
    await deps.sleep(pollMs);
  }
}

/** Worker/coord service running under the native platform manager? */
export function statusServiceLoaded(label: string): boolean {
  const worker = label === WORKER_LABEL;
  switch (process.platform) {
    case "linux":
      return captureStatusCommand([
        "systemctl",
        "--user",
        "is-active",
        worker ? WORKER_UNIT : COORD_UNIT,
      ]).exit === 0;
    case "darwin": {
      const uid = process.getuid?.() ?? "";
      return captureStatusCommand(["launchctl", "print", `gui/${uid}/${label}`]).exit === 0;
    }
    case "win32": {
      try {
        const service = runWindowsHelperSync<WindowsServiceSnapshot>(
          "service-query",
          [worker ? WINDOWS_SERVICE_NAMES.worker : WINDOWS_SERVICE_NAMES.coordinator, "basic"],
        );
        return service.state === "running";
      } catch {
        return false;
      }
    }
    default:
      throw new Error(`unsupported status platform: ${process.platform}`);
  }
}

export { COORD_LABEL as STATUS_COORD_LABEL, WORKER_LABEL as STATUS_WORKER_LABEL };
