// `roost add-machine` — mint one worker token and print the platform-specific,
// copy-paste enrollment command. Run only on the coordinator.

import {
  buildMachineJoinCommand,
  machinePlatformLabel,
} from "@roost/shared/machine-join-command";
import type { SupportedHostPlatform } from "@roost/shared/platform";
import { mintWorkerBootstrap } from "./api.ts";
import { resolveTailscale } from "./status.ts";

function strFlag(args: string[], name: string): string | undefined {
  const i = args.indexOf(name);
  if (i < 0 || i + 1 >= args.length) return undefined;
  return args[i + 1];
}

function targetPlatform(args: string[]): SupportedHostPlatform {
  switch (strFlag(args, "--platform")?.toLowerCase()) {
    case "mac":
    case "macos":
    case "darwin":
      return "darwin";
    case "linux":
      return "linux";
    case "windows":
    case "win32":
      return "win32";
    default:
      console.error("ERROR: --platform must be macos, linux, or windows.");
      process.exit(1);
  }
}

export async function addMachine(args: string[]): Promise<void> {
  const platform = targetPlatform(args);
  const label = strFlag(args, "--label") ?? "";
  const publisher = platform === "win32"
    ? strFlag(args, "--publisher-sha256") ?? process.env.ROOST_WINDOWS_PUBLISHER_SHA256
    : undefined;
  if (platform === "win32" && !/^[0-9a-f]{64}$/i.test(publisher?.trim() ?? "")) {
    console.error("ERROR: Windows enrollment requires --publisher-sha256 with the trusted release-publisher certificate SHA-256.");
    process.exit(1);
  }

  const { state, fqdn } = resolveTailscale();
  if (state !== "Running" || !fqdn) {
    console.error("ERROR: Tailscale not running / not on the coordinator host.");
    console.error("  Run `roost add-machine` on the coordinator machine with `tailscale up`.");
    process.exit(1);
  }
  const coordUrl = `https://${fqdn}:4102`;

  // Keep stdout a copy-pasteable command even though key loading logs.
  const realLog = console.log;
  console.log = ((...values: unknown[]) => console.error(...values)) as typeof console.log;
  let token: string;
  try {
    token = await mintWorkerBootstrap(label, coordUrl);
  } finally {
    console.log = realLog;
  }

  const command = buildMachineJoinCommand(platform, coordUrl, token, label, publisher);
  console.log(`Run this on the new ${machinePlatformLabel(platform)} (Tailscale must be running there):`);
  console.log("");
  console.log(command);
  console.log("");
  console.log("The token is one-shot and expires in 24h.");
}
