// Erasure of a one-shot env entry from the installed worker service definition:
// the LaunchAgent plist on macOS, the systemd --user unit on Linux. Callers are
// the bootstrap-token redemption (install.ts) and the keeper force-live retire
// authorization spent at boot (main.ts). Depends on @roost/shared paths and
// platform resolution only, so no boot subsystem is pulled in to erase a key.

import { log } from "@roost/shared/log";
import {
  supportedHostPlatform,
  type SupportedHostPlatform,
} from "@roost/shared/platform";
import { workerServicePath } from "@roost/shared/paths";
import { chmod, readFile, rename, writeFile } from "node:fs/promises";

/** Named once for the worker: the config read and the boot-time erasure below
 * must address the same installed entry. It lives HERE, not in config.ts,
 * because config.ts resolves host paths at module scope — importing the name
 * alone must not evaluate another platform's directory layout. */
export const KEEPER_FORCE_LIVE_RETIRE_ENV = "ROOST_KEEPER_FORCE_LIVE_RETIRE";

/** Remove one `KEY=value` env entry from the installed service definition.
 * Returns false when the definition never carried it, which is how a value
 * supplied by an ambient shell rather than the service manager reads. */
export async function scrubServiceDefinitionEnv(
  key: string,
  path: string = process.env.ROOST_WORKER_SERVICE_PATH ?? workerServicePath(),
  platform: SupportedHostPlatform = supportedHostPlatform(),
): Promise<boolean> {
  if (platform === "win32") return false;
  // The key is interpolated into a pattern, so a value that is not a plain env
  // name is refused instead of compiled into one.
  if (!/^[A-Z][A-Z0-9_]*$/.test(key)) {
    throw new Error(`not a service definition env key: ${key}`);
  }
  const raw = await readFile(path, "utf8");
  let next: string;
  if (platform === "darwin") {
    next = raw.replace(
      new RegExp(`\\s*<key>${key}</key>\\s*<string>[^<]*</string>`),
      "",
    );
  } else {
    const entry = new RegExp(`^\\s*Environment=(?:")?${key}=`);
    next = raw.split("\n").filter((line) => !entry.test(line)).join("\n");
  }
  if (next === raw) return false;
  const temp = `${path}.${process.pid}.env-scrub`;
  await writeFile(temp, next, { mode: 0o600 });
  await rename(temp, path);
  await chmod(path, 0o600);
  if (platform === "linux") {
    // env: process.env — Bun.spawn resolves argv[0] against the PATH in the env
    // it is handed, and with no env it uses a cached environ snapshot rather
    // than live process.env. Without this the lookup ignores a PATH set after
    // process start, which is how the scrub tests inject their fake systemctl
    // (it found the host's real systemctl on Linux and nothing at all on macOS).
    const reload = Bun.spawn(["systemctl", "--user", "daemon-reload"], {
      env: process.env as Record<string, string>,
      stdout: "ignore",
      stderr: "ignore",
    });
    await reload.exited;
  }
  return true;
}

/** The force-live authorization ends every PTY a keeper holds, so it is valid
 * for exactly the activation that received it: a value left in the service
 * definition would re-authorize that destruction on every later restart, long
 * after the operator who typed `--force-live` stopped watching. */
export async function spendKeeperForceLiveRetireAuthorization(
  path: string = process.env.ROOST_WORKER_SERVICE_PATH ?? workerServicePath(),
  platform: SupportedHostPlatform = supportedHostPlatform(),
): Promise<void> {
  try {
    const removed = await scrubServiceDefinitionEnv(
      KEEPER_FORCE_LIVE_RETIRE_ENV,
      path,
      platform,
    );
    log.warn("worker", "keeper_force_live_retire_authorization_spent", {
      service_path: path,
      removed_from_service_definition: removed,
    });
  } catch (error) {
    log.error("worker", "keeper_force_live_retire_authorization_spend_failed", {
      service_path: path,
      error: String(error),
    });
  }
}
