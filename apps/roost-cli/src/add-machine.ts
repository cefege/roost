// `roost add-machine` — mint one worker token and print the platform-specific,
// copy-paste enrollment command. Run only on the coordinator: the URL the new
// machine dials is read from this host's installed coordinator service
// definition, overlaid by the ambient environment. Roost derives no URL.

import { existsSync, readFileSync } from "node:fs";
import {
  COORDINATOR_DIAL_URL_ENV_NAMES,
  COORDINATOR_DIAL_URL_REQUIRED_MESSAGE,
  resolveCoordinatorDialUrl,
} from "@roost/shared/coordinator-dial-url";
import {
  buildMachineJoinCommand,
  machinePlatformLabel,
} from "@roost/shared/machine-join-command";
import { coordServicePath } from "@roost/shared/paths";
import type { SupportedHostPlatform } from "@roost/shared/platform";
import { mintWorkerBootstrap } from "./api.ts";
import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";
import { windowsServiceDefinitionsPath } from "./service-ctl.ts";

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

/** The dial variables as the installed coordinator service declares them.
 *  A damaged or absent definition yields no entries: the ambient environment
 *  then has to supply the URL, which is what the refusal below names. */
function installedCoordinatorDialEnv(): Record<string, string | undefined> {
  const platform = process.platform;
  const serviceFile = platform === "win32"
    ? windowsServiceDefinitionsPath()
    : coordServicePath();
  if (!existsSync(serviceFile)) return {};
  let definition: string;
  try {
    definition = readFileSync(serviceFile, "utf8");
  } catch {
    return {};
  }
  try {
    if (platform === "darwin" || platform === "linux") {
      const environment = parsePosixServiceEnvironment(definition, platform);
      return Object.fromEntries(
        COORDINATOR_DIAL_URL_ENV_NAMES.map((name) => [name, environment[name]]),
      );
    }
    const stored = JSON.parse(definition) as {
      services?: { coordinator?: { environment?: Record<string, unknown> } };
    };
    const environment = stored.services?.coordinator?.environment ?? {};
    return Object.fromEntries(
      COORDINATOR_DIAL_URL_ENV_NAMES.map((name) => {
        const value = environment[name];
        return [name, typeof value === "string" ? value : undefined];
      }),
    );
  } catch {
    return {};
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

  // The running coordinator's own definition is the ground truth for the door
  // it advertises; the environment only answers for a host with no installed
  // definition. Enrolling a machine against the wrong door is silent.
  const coordUrl = resolveCoordinatorDialUrl(installedCoordinatorDialEnv())
    ?? resolveCoordinatorDialUrl(Object.fromEntries(
      COORDINATOR_DIAL_URL_ENV_NAMES.map((name) => [name, process.env[name]]),
    ));
  if (!coordUrl) {
    console.error(`ERROR: ${COORDINATOR_DIAL_URL_REQUIRED_MESSAGE}.`);
    console.error("  Set one on this host's coordinator service, or export it for this command.");
    process.exit(1);
  }

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
  console.log(`Run this on the new ${machinePlatformLabel(platform)}:`);
  console.log("");
  console.log(command);
  console.log("");
  console.log("The token is one-shot and expires in 24h.");
}
