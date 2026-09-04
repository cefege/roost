// Coordinator target lifecycle owns admission, rollback, recovery, and final commit cleanup.
// CoordTarget delegates these transitions while snapshot transfer remains in a separate module.
// Platform branches preserve the same handoff state and service rollback guarantees.

import * as fs from "node:fs";
import { dirname, join } from "node:path";
import { roostServiceDir } from "@roost/shared/paths";
import { durableRemove, durableWriteFile } from "@roost/shared/durability";
import { log } from "@roost/shared/log";
import {
  assertWritableDirectory,
  coordLabel,
  existingDirectory,
  type CoordTargetPrepareRequest,
  type RollbackPresent,
  type CoordTargetContext,
} from "./coord-target-contracts.ts";
import { discardInflight, loadPrepared } from "./coord-target-snapshot.ts";
import {
  assertCanFrontCoordinator,
  assertNoActiveCoordinator,
  buildSourceSpa,
  captureRollback,
  captureServeConfig,
  fsyncDirectory,
  restoreServeConfig,
  rollbackSlots,
  runInstaller,
  stopRetiredCoordinator,
} from "./coord-target-posix.ts";

export async function recoverTransaction(this: CoordTargetContext): Promise<void> {
  switch (this.runtime.platform) {
    case "darwin":
    case "linux":
      return;
    case "win32":
      if (!this.windows) {
        throw new Error("Windows coordinator relocation runtime is unavailable");
      }
      await this.windows.recoverActive();
      return;
    default:
      throw new Error(`unsupported coordinator target platform: ${this.runtime.platform}`);
  }
}

export async function prepare(
  this: CoordTargetContext,
  request: CoordTargetPrepareRequest,
): Promise<void> {
  if (this.runtime.platform !== "win32") {
    discardInflight.call(this);
  }
  if (this.runtime.gitSha !== request.expected_git_sha) {
    throw new Error(`worker version ${this.runtime.gitSha} does not match coordinator`);
  }
  const target = new URL(request.target_url);
  const localDnsName = this.runtime.tailnetDnsName().toLowerCase().replace(/\.$/, "");
  if (
    target.protocol !== "https:"
    || target.port !== "4102"
    || !target.hostname.endsWith(".ts.net")
    || !localDnsName
    || target.hostname.toLowerCase() !== localDnsName
  ) {
    throw new Error("target URL does not match this worker's Tailscale address");
  }
  if (this.runtime.platform !== "win32") {
    assertWritableDirectory(this.paths.dataDir);
    assertWritableDirectory(dirname(this.paths.servicePath), true);
  }
  const stat = fs.statfsSync(existingDirectory(this.paths.dataDir));
  const available = Number(stat.bavail) * Number(stat.bsize);
  const required = Number(request.estimated_db_size) * 2 + 256 * 1024 * 1024;
  if (available < required) {
    throw new Error(`insufficient disk: required ${required}, available ${available}`);
  }
  await assertNoActiveCoordinator.call(this);
  if (this.runtime.platform !== "win32") {
    await assertCanFrontCoordinator.call(this);
  }

  if (this.runtime.platform === "win32") {
    if (!this.windows) {
      throw new Error("Windows coordinator relocation runtime is unavailable");
    }
    const windowsRelocation = await this.windows.createOperation(
      request.handoff_id,
      request.source_url,
      request.target_url,
      request.expected_git_sha,
    );
    if (request.action === "CHECK") return;
    discardInflight.call(this);
    this.prepared = {
      handoffId: request.handoff_id,
      sourceUrl: request.source_url,
      targetUrl: request.target_url,
      expectedCoordKid: request.expected_coord_kid,
      expectedGitSha: request.expected_git_sha,
      windowsRelocation,
    };
    await this.windows.admit(windowsRelocation);
    const handoffDir = join(
      roostServiceDir(),
      "data",
      "worker",
      "relocation",
      request.handoff_id,
    );
    await durableWriteFile(
      join(handoffDir, "prepared.json"),
      JSON.stringify(this.prepared),
      { platform: "win32", mode: 0o600, privateDacl: false },
    );
    return;
  }
  if (request.action === "CHECK") return;

  fs.mkdirSync(this.paths.dataDir, { recursive: true, mode: 0o700 });
  const handoffDir = join(this.paths.dataDir, "handoffs", request.handoff_id);
  fs.mkdirSync(handoffDir, { recursive: true, mode: 0o700 });
  this.prepared = {
    handoffId: request.handoff_id,
    sourceUrl: request.source_url,
    targetUrl: request.target_url,
    expectedCoordKid: request.expected_coord_kid,
    expectedGitSha: request.expected_git_sha,
  };
  captureRollback.call(this, join(handoffDir, "rollback"));
  await captureServeConfig.call(this, join(handoffDir, "rollback"));
  await stopRetiredCoordinator.call(this, request.handoff_id);
  await runInstaller.call(this, "write-plist", request.handoff_id);
  if (!process.env.ROOST_EXEC_BIN) await buildSourceSpa.call(this);
  fs.writeFileSync(
    join(handoffDir, "prepared.json"),
    JSON.stringify(this.prepared),
    { mode: 0o600 },
  );
  fsyncDirectory(handoffDir);
}

export async function abort(this: CoordTargetContext, handoffId: string): Promise<void> {
  if (this.runtime.platform === "win32") {
    if (!this.windows) {
      throw new Error("Windows coordinator relocation runtime is unavailable");
    }
    const prepared = loadPrepared.call(this, handoffId);
    if (this.inflight?.handoffId === handoffId) {
      const inflight = this.inflight;
      this.inflight = null;
      try {
        fs.closeSync(inflight.fd);
      } catch {
        // The completed transfer already closed this descriptor.
      }
      if (fs.existsSync(inflight.file)) {
        await durableRemove(inflight.file, { platform: "win32", privateDacl: true });
      }
    }
    if (prepared?.windowsRelocation) {
      await this.windows.rollback(prepared.windowsRelocation);
    }
    if (this.prepared?.handoffId === handoffId) this.prepared = null;
    fs.rmSync(
      join(roostServiceDir(), "data", "worker", "relocation", handoffId),
      { recursive: true, force: true },
    );
    return;
  }
  if (this.runtime.platform !== "darwin" && this.runtime.platform !== "linux") {
    throw new Error(`unsupported coordinator target platform: ${this.runtime.platform}`);
  }
  if (this.inflight?.handoffId === handoffId) discardInflight.call(this);
  if (this.prepared?.handoffId === handoffId) this.prepared = null;
  const handoffDir = join(this.paths.dataDir, "handoffs", handoffId);
  const rollback = join(handoffDir, "rollback");
  let present: RollbackPresent;
  try {
    present = JSON.parse(
      fs.readFileSync(join(rollback, "present.json"), "utf8"),
    ) as RollbackPresent;
  } catch {
    return;
  }
  try {
    await runInstaller.call(this, "uninstall", handoffId);
  } catch (error) {
    log.warn("coord-target", "uninstall_failed", {
      handoff_id: handoffId,
      error: String(error),
    });
  }
  for (const [canonical, name, existed] of rollbackSlots.call(this, present)) {
    const backup = join(rollback, name);
    if (existed) fs.copyFileSync(backup, canonical);
    else fs.rmSync(canonical, { force: true });
  }
  fsyncDirectory(dirname(this.paths.dbPath));
  await restoreServeConfig(rollback);
  if (present.service) {
    await this.runtime.restoreCoordService(coordLabel(), this.paths.servicePath);
  }
  fs.rmSync(handoffDir, { recursive: true, force: true });
}

export async function finalizeCommit(
  this: CoordTargetContext,
  handoffId: string,
): Promise<void> {
  switch (this.runtime.platform) {
    case "darwin":
    case "linux":
      fs.rmSync(join(this.paths.dataDir, "handoffs", handoffId), {
        recursive: true,
        force: true,
      });
      return;
    case "win32": {
      if (!this.windows) {
        throw new Error("Windows coordinator relocation runtime is unavailable");
      }
      const prepared = loadPrepared.call(this, handoffId);
      if (!prepared?.windowsRelocation) {
        throw new Error("Windows coordinator relocation was not prepared");
      }
      await this.windows.commit(prepared.windowsRelocation);
      if (this.prepared?.handoffId === handoffId) this.prepared = null;
      return;
    }
    default:
      throw new Error(`unsupported coordinator target platform: ${this.runtime.platform}`);
  }
}
