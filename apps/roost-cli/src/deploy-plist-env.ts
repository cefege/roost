// Deploy-time worker service environment: parse an installed LaunchAgent plist
// or systemd --user unit on the target, and resolve each deploy variable from
// invocation flags, that install, or ambient env. Called by the POSIX deploy
// drivers (deploy.ts, deploy-macos.ts, deploy-local.ts) and keeper-refresh.

import { failDeploy, sshExec } from "./deploy-exec.ts";
import { WORKER_UNIT } from "./service-ctl.ts";

function _unescapeXml(value: string): string {
  return value
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", "\"")
    .replaceAll("&apos;", "'")
    .replaceAll("&amp;", "&");
}

/** Parse `ROOST_*` env keys out of an existing LaunchAgent plist. Used
 *  to reuse a prior install's coord URL on subsequent deploys so the
 *  user doesn't have to re-export ROOST_COORDINATOR_URL every time.
 *  Returns an empty object if the plist or any key is absent. */
function _parsePlistEnv(plistText: string): Record<string, string> {
  const out: Record<string, string> = {};
  // `<key>NAME</key>\s*<string>VALUE</string>` — the ROOST_* keys we care about.
  const re = /<key>(ROOST_[A-Z_]+|GIT_SHA)<\/key>\s*<string>([^<]*)<\/string>/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(plistText)) !== null) {
    out[m[1]] = _unescapeXml(m[2]);
  }
  return out;
}

function _unescapeSystemd(value: string): string {
  return value.replaceAll("%%", "%").replace(/\\([\\\"nrt])/g, (_match, escaped: string) => {
    switch (escaped) {
      case "n": return "\n";
      case "r": return "\r";
      case "t": return "\t";
      default: return escaped;
    }
  });
}

/** Parse both legacy unquoted and canonical quoted systemd environment lines. */
function _parseUnitEnv(unitText: string): Record<string, string> {
  const out: Record<string, string> = {};
  const re = /^Environment=(?:"(ROOST_[A-Z_]+|GIT_SHA)=((?:\\.|[^"])*)"|(ROOST_[A-Z_]+|GIT_SHA)=([^\r\n]*))$/gm;
  let match: RegExpExecArray | null;
  while ((match = re.exec(unitText)) !== null) {
    const quoted = match[1] !== undefined;
    out[(quoted ? match[1] : match[3])!] = quoted
      ? _unescapeSystemd(match[2]!)
      : _unescapeSystemd(match[4]!.trim());
  }
  return out;
}

/** Read a single-line systemd directive from an installed unit. Resource
 * directives are not Environment entries, but are part of the same installed
 * service snapshot and must be carried into a clean staged worktree. */
export function parseSystemdServiceDirective(
  definition: string,
  name: string,
): string | undefined {
  if (!/^[A-Za-z][A-Za-z0-9]*$/.test(name)) return undefined;
  const re = new RegExp(`^${name}=([^\\r\\n]+)$`, "gm");
  let value: string | undefined;
  let match: RegExpExecArray | null;
  while ((match = re.exec(definition)) !== null) value = match[1]?.trim();
  if (!value) return undefined;
  return value.startsWith("\"") && value.endsWith("\"") ? value.slice(1, -1) : value;
}

export function parsePosixServiceEnvironment(
  definition: string,
  platform: "darwin" | "linux",
): Record<string, string> {
  return platform === "linux" ? _parseUnitEnv(definition) : _parsePlistEnv(definition);
}

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
 *  (ROOST_COORDINATOR_URL, ROOST_BOOTSTRAP_TOKEN, ROOST_DIAG_*) is fleet-wide
 *  and keeps its ambient fallback. */
const DEPLOY_IDENTITY_ENV_FLAGS: Record<string, string> = {
  ROOST_WORKER_LABEL: "--label",
  ROOST_REACHABLE_ADDR: "--reachable-addr",
};

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
export async function _backfillEnvFromPlist(host: string | "self"): Promise<HostEnvBackfill> {
  const KEYS = ["ROOST_COORDINATOR_URL", "ROOST_REACHABLE_ADDR", "ROOST_WORKER_LABEL"];
  const PLIST = "Library/LaunchAgents/com.roost.worker-v2.plist";
  const UNIT = `.config/systemd/user/${WORKER_UNIT}`;
  // Whichever the box has. Both parsers key off their own syntax, so
  // concatenating the two files (the ssh case) is unambiguous.
  let text = "";
  if (host === "self") {
    const home = process.env.HOME ?? "";
    text = (await Bun.file(`${home}/${PLIST}`).text().catch(() => ""))
      + (await Bun.file(`${home}/${UNIT}`).text().catch(() => ""));
  } else {
    const r = await sshExec(host, `cat ~/${PLIST} 2>/dev/null || true; cat ~/${UNIT} 2>/dev/null || true`);
    if (r.exit !== 0) return { env: {}, filled: [] };
    text = r.stdout;
  }
  if (!text) return { env: {}, filled: [] };
  const parsed = { ..._parseUnitEnv(text), ..._parsePlistEnv(text) };
  const env: Record<string, string> = { ...parsed };
  const filled: string[] = [];
  for (const k of KEYS) {
    if (parsed[k]) {
      env[k] = parsed[k];
      filled.push(k);
    }
  }
  return { env, filled };
}
