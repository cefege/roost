import { sshExec } from "./deploy-exec.ts";

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

/** Fill any missing ROOST_* env vars in process.env from the existing
 *  worker plist on the target box. Returns the list of keys backfilled
 *  so the caller can log them. */
export async function _backfillEnvFromPlist(host: string | "self"): Promise<string[]> {
  const KEYS = ["ROOST_COORDINATOR_URL", "ROOST_REACHABLE_ADDR", "ROOST_WORKER_LABEL"];
  const missing = KEYS.filter((k) => !process.env[k]);
  if (missing.length === 0) return [];
  let plistText = "";
  if (host === "self") {
    try {
      const home = process.env.HOME ?? "";
      const path = `${home}/Library/LaunchAgents/com.roost.worker-v2.plist`;
      plistText = await Bun.file(path).text();
    } catch { return []; }
  } else {
    const r = await sshExec(host, "cat ~/Library/LaunchAgents/com.roost.worker-v2.plist 2>/dev/null || true");
    if (r.exit !== 0) return [];
    plistText = r.stdout;
  }
  if (!plistText) return [];
  const env = _parsePlistEnv(plistText);
  const filled: string[] = [];
  for (const k of missing) {
    if (env[k]) {
      process.env[k] = env[k];
      filled.push(k);
    }
  }
  return filled;
}
