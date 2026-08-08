// `roost add-mac` — run on the COORDINATOR to print the copy-paste one-liner
// that turns a bare tailnet Mac into a registered worker. Mints a one-shot
// worker bootstrap token, resolves the coord's own tailnet URL, and emits the
// `curl … join.sh | … bash` invocation the user pastes on the new Mac.
// The pull counterpart to the push-based `roost deploy` (which stays as the
// UPDATE path for already-joined Macs).

import { mintWorkerBootstrap } from "./api.ts";
import { resolveTailscale } from "./status.ts";

/** String flag: `--label foo` → "foo", else undefined. (Copied from api.ts's
 *  module-private helper — it is not exported.) */
function strFlag(args: string[], name: string): string | undefined {
  const i = args.indexOf(name);
  if (i < 0 || i + 1 >= args.length) return undefined;
  return args[i + 1];
}

export async function addMac(args: string[]): Promise<void> {
  const label = strFlag(args, "--label") ?? "";

  const { fqdn } = resolveTailscale();
  if (!fqdn) {
    console.error("ERROR: Tailscale not running / not on the coordinator host.");
    console.error("  Run `roost add-mac` on the coordinator machine with `tailscale up`.");
    process.exit(1);
  }
  const coordUrl = `https://${fqdn}:4102`;

  // loadWorkerKey logs "worker key loaded" via the shared facade to stdout;
  // shunt console.log→stderr just while minting so stdout stays a clean,
  // copy-pasteable command (mirrors api()'s buildApiClient guard).
  const realLog = console.log;
  console.log = ((...a: unknown[]) => console.error(...a)) as typeof console.log;
  let token: string;
  try { token = await mintWorkerBootstrap(label); }
  finally { console.log = realLog; }

  const labelEnv = label ? ` ROOST_WORKER_LABEL=${JSON.stringify(label)}` : "";
  const cmd =
    `curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | ` +
    `ROOST_COORDINATOR_URL=${JSON.stringify(coordUrl)} ` +
    `ROOST_BOOTSTRAP_TOKEN=${JSON.stringify(token)}${labelEnv} bash`;

  console.log("Run this on the NEW machine (Tailscale must be running there):");
  console.log("");
  console.log(cmd);
  console.log("");
  console.log("The token is one-shot and expires in 24h.");
}
