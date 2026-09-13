// `roost join` — install/register this host's worker from a one-shot
// coordinator bootstrap token. POSIX keeps the existing source deploy flow;
// the signed Windows release installs native SCM services without bash/SSH.
import { resolve } from "node:path";
import { _deployLocal } from "./deploy-local.ts";
import { DeployFailure, resolveLocalGitShaOrDie } from "./deploy-exec.ts";
import {
  createJournaledKeeperUpdateCallbacks,
  type JournaledKeeperUpdateCallbacks,
} from "./direct-keeper-update.ts";
import {
  installWorkerAgent,
  readWindowsServiceCredentials,
} from "./install-binary-agents.ts";
import { ROOST_VERSION } from "./version.ts";
const REPO_ROOT = resolve(import.meta.dir, "..", "..", "..");
type JoinPosixDeploy = (
  host: string,
  options: {
    sourceRoot: string;
    gitSha: string;
    keeperUpdate: null;
    workerFingerprint: null;
    keeperCallbacks: JournaledKeeperUpdateCallbacks;
  },
) => Promise<void>;

export async function _deployJoinedPosixWorker(
  sourceRoot: string,
  deployLocal: JoinPosixDeploy,
  keeperCallbacks: JournaledKeeperUpdateCallbacks,
): Promise<void> {
  await deployLocal("this machine", {
    sourceRoot,
    gitSha: _resolveJoinGitShaOrDie(sourceRoot),
    keeperUpdate: null,
    workerFingerprint: null,
    keeperCallbacks,
  });
}

export function _resolveJoinGitShaOrDie(sourceRoot: string = REPO_ROOT): string {
  const gitSha = resolveLocalGitShaOrDie(sourceRoot);
  if (gitSha.endsWith("-dirty")) {
    throw new DeployFailure(7, "a joined worker requires a clean committed source snapshot");
  }
  return gitSha;
}

export async function join(args: string[]): Promise<void> {
  const coordUrl = process.env.ROOST_COORDINATOR_URL;
  if (!coordUrl) {
    console.error("ERROR: ROOST_COORDINATOR_URL required — get the join command from");
    console.error("  `roost add-machine --platform windows` on your coordinator (or Settings → Machines → Add machine).");
    process.exit(1);
  }
  const bootstrapToken = process.env.ROOST_BOOTSTRAP_TOKEN;
  if (!bootstrapToken) {
    console.error("ERROR: ROOST_BOOTSTRAP_TOKEN required — get the join command from");
    console.error("  `roost add-machine --platform windows` on your coordinator (or Settings → Machines → Add machine).");
    process.exit(1);
  }
  const keeperCallbacks = createJournaledKeeperUpdateCallbacks();

  switch (process.platform) {
    case "darwin":
    case "linux":
      await _deployJoinedPosixWorker(REPO_ROOT, _deployLocal, keeperCallbacks);
      break;
    case "win32": {
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
