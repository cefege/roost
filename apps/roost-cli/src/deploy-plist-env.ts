// Deploy-time worker service environment: parse an installed LaunchAgent plist
// or systemd --user unit on the target, and resolve each deploy variable from
// invocation flags, that install, or ambient env. Called by the POSIX deploy
// drivers (deploy.ts, deploy-macos.ts, deploy-local.ts) and keeper-refresh.

import { failDeploy, sshExec } from "./deploy-exec.ts";
import { WORKER_UNIT } from "./service-ctl.ts";
import {
  parsePosixServiceEnvironment,
  parseSystemdServiceDirective,
} from "./worker-service-runtime.ts";

export {
  parsePosixServiceEnvironment,
  parseSystemdServiceDirective,
};

export interface HostEnvBackfill {
  /** Values read from this host's own service definition. Never merged into
   *  process.env: `roost push` deploys every target in ONE process, so a
   *  global write leaks one machine's identity into the next machine's
   *  install. */
  env: Record<string, string>;
  /** Keys found in the target's installed service definition. */
  filled: string[];
}

export type DeployEnvTarget = "self" | "remote";

/** Env keys that name ONE machine, mapped to the `roost deploy` flag that
 *  supplies each. A remote deploy resolves an identity key only from that flag
 *  or from the target's own installed service definition: the deploying
 *  shell's ambient value describes the box running the CLI, so adopting it
 *  installs the target under another machine's label and reachable address —
 *  the coordinator then lists two workers with one name and the target's real
 *  identity vanishes from the fleet. Every other deploy key
 *  (ROOST_COORDINATOR_URL, ROOST_BOOTSTRAP_TOKEN, ROOST_DIAG_*, and the
 *  host-local settings below) keeps its ambient fallback. */
const DEPLOY_IDENTITY_ENV_FLAGS: Record<string, string> = {
  ROOST_WORKER_LABEL: "--label",
  ROOST_REACHABLE_ADDR: "--reachable-addr",
};

/** Worker settings that belong to whichever machine runs the worker: the
 *  loopback bind of its local UI door and the browser origins that door
 *  admits. Neither names the machine in the fleet nor a secret, so they follow
 *  the non-identity rule above — the target's installed value wins and the
 *  deploying shell only seeds a first install. A deploy that dropped them
 *  would silently move an operator's local UI door back to its default port
 *  and shut out the browsers it was reachable from. ROOST_WEB_DIST_PATH is
 *  deliberately NOT here: it points INTO a release directory, so carrying the
 *  installed value forward names the release the next settlement deletes. Each
 *  deploy stamps the dist it just built (deploy-linux.ts, deploy-macos.ts,
 *  deploy-local.ts). */
export const DEPLOY_HOST_LOCAL_ENV_KEYS = [
  "ROOST_WORKER_LOCAL_UI_BIND",
  "ROOST_WORKER_LOCAL_UI_ALLOWED_ORIGINS",
] as const;

export interface DeployIdentityInvocation {
  workerLabel?: string;
  reachableAddr?: string;
}

/** Resolve one deploy variable without mutating ambient state. `target` says
 *  whose machine the ambient env describes: for "self" it is the target, for
 *  "remote" it is a different box and cannot supply an identity key. */
export function _resolveDeployEnvValue(
  key: string,
  installedEnv: Record<string, string>,
  invocationValue: string | undefined,
  target: DeployEnvTarget,
): string | undefined {
  const installed = Object.hasOwn(installedEnv, key) ? installedEnv[key] : undefined;
  const selected = invocationValue ?? installed;
  if (selected !== undefined) return selected;
  // A remote target's ambient env belongs to the deploying box, never to it.
  if (target === "remote" && Object.hasOwn(DEPLOY_IDENTITY_ENV_FLAGS, key)) {
    return undefined;
  }
  return process.env[key];
}

/** Identity overrides for a remote install. An unresolvable key is left unset
 *  so the target derives its own hostname / tailnet name, EXCEPT when the
 *  deploying shell exports that key: guessing which machine the operator meant
 *  is what mislabels a fleet, so the deploy refuses and names the flag. */
export function resolveRemoteDeployIdentityEnv(
  host: string,
  installedEnv: Record<string, string>,
  invocation: DeployIdentityInvocation = {},
): Record<string, string | undefined> {
  const invocationValues: Record<string, string | undefined> = {
    ROOST_WORKER_LABEL: invocation.workerLabel,
    ROOST_REACHABLE_ADDR: invocation.reachableAddr,
  };
  const identity: Record<string, string | undefined> = {};
  for (const [key, flag] of Object.entries(DEPLOY_IDENTITY_ENV_FLAGS)) {
    const value = _resolveDeployEnvValue(key, installedEnv, invocationValues[key], "remote");
    if (value === undefined && process.env[key] !== undefined) {
      failDeploy(
        6,
        `${key} in this shell names the machine running roost deploy, not ${host}`
          + ` (no prior install on ${host} to reuse).`
          + ` Supply the target's own identity with ${flag}=<value>,`
          + ` or unset ${key} so ${host} derives its own.`,
      );
    }
    identity[key] = value;
  }
  return identity;
}

/** Read deploy env vars from the existing worker service definition on the
 *  target box — the LaunchAgent plist on macOS, the systemd --user unit on
 *  Linux. All identity keys are always read so ambient values cannot hide an
 *  enrolled target's installed identity. */
/** Reads the installed worker definition without interpreting its command. */
export async function _readWorkerServiceDefinition(
  host: string | "self",
): Promise<string | null> {
  const plist = "Library/LaunchAgents/com.roost.worker-v2.plist";
  const unit = `.config/systemd/user/${WORKER_UNIT}`;
  if (host === "self") {
    const home = process.env.HOME ?? "";
    const text = (await Bun.file(`${home}/${plist}`).text().catch(() => ""))
      + (await Bun.file(`${home}/${unit}`).text().catch(() => ""));
    return text || null;
  }
  const result = await sshExec(
    host,
    `cat ~/${plist} 2>/dev/null || true; cat ~/${unit} 2>/dev/null || true`,
  );
  return result.exit === 0 && result.stdout ? result.stdout : null;
}

export async function _backfillEnvFromPlist(host: string | "self"): Promise<HostEnvBackfill> {
  const KEYS = [
    "ROOST_COORDINATOR_URL",
    "ROOST_REACHABLE_ADDR",
    "ROOST_WORKER_LABEL",
    ...DEPLOY_HOST_LOCAL_ENV_KEYS,
  ];
  const text = await _readWorkerServiceDefinition(host);
  if (!text) return { env: {}, filled: [] };
  const parsed = {
    ...parsePosixServiceEnvironment(text, "linux"),
    ...parsePosixServiceEnvironment(text, "darwin"),
  };
  const env: Record<string, string> = { ...parsed };
  const filled: string[] = [];
  for (const key of KEYS) {
    if (parsed[key]) {
      env[key] = parsed[key];
      filled.push(key);
    }
  }
  return { env, filled };
}
