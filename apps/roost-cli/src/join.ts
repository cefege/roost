// `roost join` — install/register this host's worker from a one-shot
// coordinator bootstrap token. POSIX keeps the existing source deploy flow;
// the signed Windows release installs native SCM services without bash/SSH.
import { _deployLocal } from "./deploy-local.ts";
import {
  installWorkerAgent,
  readWindowsServiceCredentials,
} from "./install-binary-agents.ts";
import { ROOST_VERSION } from "./version.ts";

export async function join(args: string[]): Promise<void> {
  const coordUrl = process.env.ROOST_COORDINATOR_URL;
  if (!coordUrl) {
    console.error("ERROR: ROOST_COORDINATOR_URL required — get the join command from");
    console.error("  `roost add-mac` on your coordinator (or Settings → Machines → Add machine).");
    process.exit(1);
  }
  const bootstrapToken = process.env.ROOST_BOOTSTRAP_TOKEN;
  if (!bootstrapToken) {
    console.error("ERROR: ROOST_BOOTSTRAP_TOKEN required — get the join command from");
    console.error("  `roost add-mac` on your coordinator (or Settings → Machines → Add machine).");
    process.exit(1);
  }

  switch (process.platform) {
    case "darwin":
    case "linux":
      await _deployLocal("this machine");
      break;
    case "win32": {
      if (!Bun.which("tailscale.exe") && !Bun.which("tailscale")) {
        throw new Error("Tailscale is required — install and connect it before running join.ps1");
      }
      if (!args.includes("--windows-service-credential-stdin")) {
        throw new Error("Windows join requires the framed service credential from the signed join.ps1 front door");
      }
      const credentials = await readWindowsServiceCredentials();
      try {
        await installWorkerAgent({
          execPath: process.execPath,
          coordUrl,
          bootstrapToken,
          gitSha: ROOST_VERSION,
          coordinatorHost: false,
          credentials,
          log: (message) => console.log(`>> ${message}`),
        });
      } finally {
        credentials.password = undefined;
      }
      break;
    }
    default:
      throw new Error(`unsupported join platform: ${process.platform}`);
  }

  console.log("");
  console.log("Joined. This machine should appear in Settings → Machines within a few seconds.");
  console.log(process.platform === "win32"
    ? "  check: roost status"
    : "  check: bun apps/roost-cli/src/main.ts status");
}
