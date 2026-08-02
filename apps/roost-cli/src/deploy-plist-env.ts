import { sshExec } from "./deploy-exec.ts";
import { WORKER_UNIT } from "./service-ctl.ts";

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
    out[m[1]] = m[2];
  }
  return out;
}

/** Same, for a systemd --user unit: `Environment=NAME=VALUE` lines. */
function _parseUnitEnv(unitText: string): Record<string, string> {
  const out: Record<string, string> = {};
  const re = /^Environment=(ROOST_[A-Z_]+|GIT_SHA)=(.*)$/gm;
  let m: RegExpExecArray | null;
  while ((m = re.exec(unitText)) !== null) {
    out[m[1]] = m[2].trim();
  }
  return out;
}

export interface HostEnvBackfill {
  /** Values read off this host's own service definition. Never merged into
   *  process.env: `roost push` deploys every target in ONE process, so a
   *  global write leaks one machine's identity into the next machine's
   *  install — ROOST_WORKER_LABEL and ROOST_REACHABLE_ADDR are per-host. */
  env: Record<string, string>;
  /** Keys that came from the target rather than the caller's environment. */
  filled: string[];
}

/** Resolve one deploy variable without mutating ambient state. An enrolled
 *  worker's installed coordinator is authoritative; other per-host values
 *  remain explicit caller overrides. */
export function _resolveDeployEnvValue(
  key: string,
  installedEnv: Record<string, string>,
): string | undefined {
  const ambient = process.env[key];
  return key === "ROOST_COORDINATOR_URL"
    ? installedEnv[key] ?? ambient
    : ambient ?? installedEnv[key];
}

/** Read deploy env vars from the existing worker service definition on the
 *  target box — the LaunchAgent plist on macOS, the systemd --user unit on
 *  Linux. The installed coordinator URL is always read because it is
 *  authoritative for an enrolled worker; other keys are read only when
 *  absent from the caller environment. */
export async function _backfillEnvFromPlist(host: string | "self"): Promise<HostEnvBackfill> {
  const KEYS = ["ROOST_COORDINATOR_URL", "ROOST_REACHABLE_ADDR", "ROOST_WORKER_LABEL"];
  const requested = KEYS.filter((k) => k === "ROOST_COORDINATOR_URL" || !process.env[k]);
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
  const env: Record<string, string> = {};
  const filled: string[] = [];
  for (const k of requested) {
    if (parsed[k]) {
      env[k] = parsed[k];
      filled.push(k);
    }
  }
  return { env, filled };
}
