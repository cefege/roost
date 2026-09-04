// POSIX coordinator relocation operations own rollback files, service installation, and probes.
// Lifecycle and snapshot stages call them with the CoordTarget state that owns one handoff.
// Installer output and durable directory ordering remain visible at the promotion boundary.

import * as fs from "node:fs";
import { join } from "node:path";
import { COORD_INSTALL_SH } from "@roost/shared/install-scripts";
import { log } from "@roost/shared/log";
import {
  RELOCATION_INSTALLER_TIMEOUT_MS,
  captureServeConfig as runCaptureServeConfig,
  restoreServeConfig as runRestoreServeConfig,
  assertCanFrontCoordinator as probeCanFrontCoordinator,
  settleWithinTimeout,
} from "./coord-target-spawns.ts";
import {
  coordLabel,
  defaultCoordHealthy,
  type CoordTargetContext,
  type RollbackPresent,
} from "./coord-target-contracts.ts";

export async function assertRelocatedCoordReachable(
  this: CoordTargetContext,
  targetUrl: string,
): Promise<void> {
  const probe = this.runtime.coordHealthy ?? defaultCoordHealthy;
  for (let attempt = 0; attempt < 5; attempt++) {
    if (attempt > 0) await Bun.sleep(2_000);
    if (await probe(targetUrl)) return;
  }
  throw new Error(`relocated coordinator is not reachable at ${targetUrl}`);
}

export function rollbackSlots(
  this: CoordTargetContext,
  present: RollbackPresent,
): readonly (readonly [string, string, boolean])[] {
  return [
    [this.paths.dbPath, "coordinator_v2.db", present.db],
    [this.paths.keyPath, "ssh_ed25519.key", present.key],
    [this.paths.authorizedKeysPath, "authorized_keys.roost", present.authorizedKeys],
    [this.paths.handoffPath, "coord-handoff.json", present.handoff],
    [this.paths.servicePath, "coordinator.service-definition", present.service],
  ] as const;
}

export function captureRollback(this: CoordTargetContext, rollback: string): void {
  if (fs.existsSync(join(rollback, "present.json"))) return;
  fs.mkdirSync(rollback, { recursive: true, mode: 0o700 });
  const present: RollbackPresent = {
    db: fs.existsSync(this.paths.dbPath),
    key: fs.existsSync(this.paths.keyPath),
    authorizedKeys: fs.existsSync(this.paths.authorizedKeysPath),
    handoff: fs.existsSync(this.paths.handoffPath),
    service: fs.existsSync(this.paths.servicePath),
  };
  for (const [canonical, name, exists] of rollbackSlots.call(this, present)) {
    if (exists) fs.copyFileSync(canonical, join(rollback, name));
  }
  fs.writeFileSync(join(rollback, "present.json"), JSON.stringify(present), { mode: 0o600 });
  fsyncDirectory(rollback);
}

export async function captureServeConfig(this: CoordTargetContext, rollback: string): Promise<void> {
  await runCaptureServeConfig(rollback);
  fsyncDirectory(rollback);
}

export async function restoreServeConfig(rollback: string): Promise<void> {
  await runRestoreServeConfig(rollback);
}

export async function assertNoActiveCoordinator(this: CoordTargetContext): Promise<boolean> {
  if (!await this.runtime.isCoordServiceActive(coordLabel())) return false;
  try {
    const local = JSON.parse(fs.readFileSync(this.paths.handoffPath, "utf8")) as {
      role?: string;
      phase?: string;
    };
    if (local.role === "SOURCE" && local.phase === "COMMITTED") return true;
  } catch {
    // No readable handoff file means this is a plain active coordinator.
  }
  throw new Error("target already has an active coordinator");
}

export async function assertCanFrontCoordinator(this: CoordTargetContext): Promise<void> {
  await probeCanFrontCoordinator(this.runtime.platform);
}

export async function stopRetiredCoordinator(
  this: CoordTargetContext,
  handoffId: string,
): Promise<void> {
  if (!await this.runtime.isCoordServiceActive(coordLabel())) return;
  await runInstaller.call(this, "uninstall", handoffId);
  log.info("coord-target", "retired_coordinator_stopped", { handoff_id: handoffId });
}

export async function buildSourceSpa(this: CoordTargetContext): Promise<void> {
  const webDir = join(process.cwd(), "apps", "web");
  const child = Bun.spawn(["bun", "run", "build"], {
    cwd: webDir,
    env: { ...process.env, ROOST_GIT_SHA: this.runtime.gitSha },
    stdout: "pipe",
    stderr: "pipe",
  });
  const [out, err, code] = await settleWithinTimeout(
    child,
    `bun run build (${webDir})`,
    RELOCATION_INSTALLER_TIMEOUT_MS,
    () => Promise.all([
      new Response(child.stdout).text(),
      new Response(child.stderr).text(),
      child.exited,
    ]),
  );
  if (code !== 0) {
    const detail = `${err}\n${out}`.trim().split("\n").filter(Boolean).slice(-3).join("; ");
    throw new Error(
      `target web build failed in ${webDir} (exit ${code})${detail ? `: ${detail}` : ""}`,
    );
  }
}

export async function runInstaller(
  this: CoordTargetContext,
  command: "write-plist" | "install" | "uninstall",
  handoffId: string,
): Promise<void> {
  if (this.runtime.platform === "win32") {
    throw new Error("Windows coordinator promotion must use the SCM relocation transaction");
  }
  if (this.runtime.platform !== "darwin" && this.runtime.platform !== "linux") {
    throw new Error(`unsupported coordinator target platform: ${this.runtime.platform}`);
  }
  const dir = join(this.paths.dataDir, "handoffs", handoffId);
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const script = process.env.ROOST_COORDINATOR_INSTALL_SCRIPT
    ?? (COORD_INSTALL_SH
      ? join(dir, "install-coordinator.sh")
      : join(process.cwd(), "apps", "coord", "scripts", "install.sh"));
  if (COORD_INSTALL_SH && !process.env.ROOST_COORDINATOR_INSTALL_SCRIPT) {
    fs.writeFileSync(script, COORD_INSTALL_SH, { mode: 0o700 });
  }
  const child = Bun.spawn(["bash", script, command], {
    env: {
      ...process.env,
      ROOST_REPO_ROOT: process.cwd(),
      ROOST_COORD_DATA_DIR: this.paths.dataDir,
      ROOST_COORDINATOR_DB: this.paths.dbPath,
      ROOST_COORDINATOR_KEY_PATH: this.paths.keyPath,
      ROOST_COORDINATOR_AUTHORIZED_KEYS: this.paths.authorizedKeysPath,
      ROOST_COORDINATOR_HANDOFF_PATH: this.paths.handoffPath,
      ROOST_COORDINATOR_PUBLIC_URL:
        this.prepared?.targetUrl ?? process.env.ROOST_COORDINATOR_PUBLIC_URL,
      ROOST_COORD_PLIST: this.paths.servicePath,
      ROOST_COORD_UNIT: this.paths.servicePath,
      ROOST_GIT_SHA: this.runtime.gitSha,
      ROOST_EXEC_BIN: process.env.ROOST_EXEC_BIN,
      ROOST_SKIP_ENV_LOCAL: "1",
    },
    stdout: "pipe",
    stderr: "pipe",
  });
  const [out, err, code] = await settleWithinTimeout(
    child,
    `install.sh ${command}`,
    RELOCATION_INSTALLER_TIMEOUT_MS,
    () => Promise.all([
      new Response(child.stdout).text(),
      new Response(child.stderr).text(),
      child.exited,
    ]),
  );
  if (code !== 0) {
    const detail = `${err}\n${out}`.trim().split("\n").filter(Boolean).slice(-4).join("; ");
    throw new Error(
      `coordinator ${command} failed (exit ${code})${detail ? `: ${detail}` : ` running ${script}`}`,
    );
  }
}

export function fsyncDirectory(path: string): void {
  const fd = fs.openSync(path, "r");
  try {
    fs.fsyncSync(fd);
  } finally {
    fs.closeSync(fd);
  }
}
