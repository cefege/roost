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

/** Fill any missing ROOST_* env vars in process.env from the existing
 *  worker service definition on the target box — the LaunchAgent plist on
 *  macOS, the systemd --user unit on Linux. Returns the list of keys
 *  backfilled so the caller can log them. */
export async function _backfillEnvFromPlist(host: string | "self"): Promise<string[]> {
  const KEYS = ["ROOST_COORDINATOR_URL", "ROOST_REACHABLE_ADDR", "ROOST_WORKER_LABEL"];
  const missing = KEYS.filter((k) => !process.env[k]);
  if (missing.length === 0) return [];
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
    if (r.exit !== 0) return [];
    text = r.stdout;
  }
  if (!text) return [];
  const env = { ..._parseUnitEnv(text), ..._parsePlistEnv(text) };
  const filled: string[] = [];
  for (const k of missing) {
    if (env[k]) {
      process.env[k] = env[k];
      filled.push(k);
    }
  }
  return filled;
}
