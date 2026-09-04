// Coordinator snapshot transfer owns prepared-state reload, chunk ordering, and promotion staging.
// CoordTarget delegates START and append operations while retaining the active handoff state.
// Validation completes before POSIX renames or the Windows service transaction can become live.

import * as fs from "node:fs";
import { dirname, join } from "node:path";
import { roostServiceDir } from "@roost/shared/paths";
import { durableRemove, durableWriteFile, flushDurablePath } from "@roost/shared/durability";
import {
  coordinatorKeyFingerprint,
  dashboardIdFromVerifiedSnapshot,
  targetHandoffPayload,
  type CoordTargetSnapshotChunk,
  type CoordTargetStartSnapshotRequest,
  type CoordTargetContext,
  type PreparedTarget,
} from "./coord-target-contracts.ts";
import {
  assertRelocatedCoordReachable,
  fsyncDirectory,
  runInstaller,
} from "./coord-target-posix.ts";

export function loadPrepared(
  this: CoordTargetContext,
  handoffId: string,
): PreparedTarget | null {
  if (this.prepared?.handoffId === handoffId) return this.prepared;
  try {
    const handoffDir = this.runtime.platform === "win32"
      ? join(roostServiceDir(), "data", "worker", "relocation", handoffId)
      : join(this.paths.dataDir, "handoffs", handoffId);
    const parsed = JSON.parse(
      fs.readFileSync(join(handoffDir, "prepared.json"), "utf8"),
    ) as PreparedTarget;
    if (parsed.handoffId !== handoffId) return null;
    this.prepared = parsed;
    return parsed;
  } catch {
    return null;
  }
}

export function discardInflight(this: CoordTargetContext): void {
  const inflight = this.inflight;
  if (!inflight) return;
  this.inflight = null;
  try {
    fs.closeSync(inflight.fd);
  } catch {
    // The completed transfer already closed this descriptor.
  }
  fs.rmSync(inflight.file, { force: true });
}

export function startSnapshot(
  this: CoordTargetContext,
  request: CoordTargetStartSnapshotRequest,
): void | Promise<void> {
  if (this.runtime.platform === "win32") {
    return startWindowsSnapshot.call(this, request);
  }
  if (this.runtime.platform !== "darwin" && this.runtime.platform !== "linux") {
    throw new Error(`unsupported coordinator target platform: ${this.runtime.platform}`);
  }
  const prepared = loadPrepared.call(this, request.handoff_id);
  if (!prepared) throw new Error("coordinator target was not prepared");
  if (this.inflight) {
    if (this.inflight.handoffId !== request.handoff_id) {
      throw new Error("another coordinator snapshot is already in flight");
    }
    discardInflight.call(this);
  }
  const dir = join(this.paths.dataDir, "handoffs", request.handoff_id);
  const file = join(dir, "coordinator_v2.snapshot");
  fs.rmSync(file, { force: true });
  fs.writeFileSync(join(dir, "ssh_ed25519.key"), request.coord_key_pem, { mode: 0o600 });
  fs.writeFileSync(join(dir, "authorized_keys.roost"), request.authorized_keys, { mode: 0o600 });
  this.inflight = {
    handoffId: request.handoff_id,
    file,
    fd: fs.openSync(file, "wx", 0o600),
    expectedSize: Number(request.total_size),
    expectedSha256: request.sha256,
    expectedWorkerFps: request.expected_worker_fps,
    secretSha256: request.secret_sha256,
    nextSeq: 0,
    received: 0,
    hasher: new Bun.CryptoHasher("sha256"),
  };
}

async function startWindowsSnapshot(
  this: CoordTargetContext,
  request: CoordTargetStartSnapshotRequest,
): Promise<void> {
  if (!this.windows) {
    throw new Error("Windows coordinator relocation runtime is unavailable");
  }
  const prepared = loadPrepared.call(this, request.handoff_id);
  if (!prepared?.windowsRelocation) {
    throw new Error("Windows coordinator target was not prepared");
  }
  try {
    if (this.inflight) {
      if (this.inflight.handoffId !== request.handoff_id) {
        throw new Error("another coordinator snapshot is already in flight");
      }
      const previous = this.inflight;
      this.inflight = null;
      try {
        fs.closeSync(previous.fd);
      } catch {
        // The prior attempt may have completed its descriptor flush.
      }
      if (fs.existsSync(previous.file)) {
        await durableRemove(previous.file, { platform: "win32", privateDacl: true });
      }
    }
    const dir = join(
      roostServiceDir(),
      "data",
      "worker",
      "relocation",
      request.handoff_id,
    );
    const file = join(dir, "coordinator_v2.snapshot");
    if (fs.existsSync(file)) {
      await durableRemove(file, { platform: "win32", privateDacl: true });
    }
    await durableWriteFile(join(dir, "ssh_ed25519.key"), request.coord_key_pem, {
      platform: "win32",
      mode: 0o600,
      privateDacl: false,
    });
    await durableWriteFile(join(dir, "authorized_keys.roost"), request.authorized_keys, {
      platform: "win32",
      mode: 0o600,
      privateDacl: false,
    });
    const fd = fs.openSync(file, "wx", 0o600);
    this.inflight = {
      handoffId: request.handoff_id,
      file,
      fd,
      expectedSize: Number(request.total_size),
      expectedSha256: request.sha256,
      expectedWorkerFps: request.expected_worker_fps,
      secretSha256: request.secret_sha256,
      nextSeq: 0,
      received: 0,
      hasher: new Bun.CryptoHasher("sha256"),
    };
  } catch (error) {
    await this.abort(request.handoff_id).catch(() => {});
    throw error;
  }
}

export async function appendSnapshot(
  this: CoordTargetContext,
  chunk: CoordTargetSnapshotChunk,
): Promise<void> {
  const inflight = this.inflight;
  if (!inflight || inflight.handoffId !== chunk.handoff_id) {
    throw new Error("unknown coordinator snapshot");
  }
  const dir = dirname(inflight.file);
  try {
    if (chunk.seq < inflight.nextSeq) return;
    if (chunk.seq !== inflight.nextSeq) {
      throw new Error(
        `snapshot chunk out of order: expected ${inflight.nextSeq}, got ${chunk.seq}`,
      );
    }
    let offset = 0;
    while (offset < chunk.data.length) {
      const written = fs.writeSync(
        inflight.fd,
        chunk.data,
        offset,
        chunk.data.length - offset,
      );
      if (written === 0) throw new Error("coordinator snapshot write made no progress");
      offset += written;
    }
    inflight.hasher.update(chunk.data);
    inflight.received += chunk.data.length;
    inflight.nextSeq += 1;
    if (!chunk.last) return;

    fs.fsyncSync(inflight.fd);
    fs.closeSync(inflight.fd);
    this.inflight = null;
    if (this.runtime.platform === "win32") {
      await flushDurablePath(inflight.file, { platform: "win32" });
    }
    const actualSha256 = inflight.hasher.digest("hex");
    if (
      inflight.received !== inflight.expectedSize
      || actualSha256 !== inflight.expectedSha256
    ) {
      throw new Error("coordinator snapshot checksum mismatch");
    }
    const dashboardId = dashboardIdFromVerifiedSnapshot(
      inflight.file,
      inflight.expectedWorkerFps,
    );
    const prepared = loadPrepared.call(this, chunk.handoff_id);
    if (!prepared) throw new Error("coordinator target was not prepared");
    const keyFingerprint = await coordinatorKeyFingerprint(
      fs.readFileSync(join(dir, "ssh_ed25519.key")),
    );
    if (keyFingerprint !== prepared.expectedCoordKid) {
      throw new Error("coordinator key fingerprint does not match source");
    }
    if (this.runtime.platform === "win32") {
      if (!this.windows || !prepared.windowsRelocation) {
        throw new Error("Windows coordinator relocation admission is unavailable");
      }
      await durableWriteFile(
        join(dir, "target-handoff.json"),
        targetHandoffPayload(prepared, inflight, dashboardId),
        { platform: "win32", mode: 0o600, privateDacl: false },
      );
      await this.windows.apply(prepared.windowsRelocation);
      return;
    }
    if (this.runtime.platform !== "darwin" && this.runtime.platform !== "linux") {
      throw new Error(`unsupported coordinator target platform: ${this.runtime.platform}`);
    }
    for (const suffix of ["-wal", "-shm"]) {
      fs.rmSync(`${this.paths.dbPath}${suffix}`, { force: true });
    }
    fs.writeFileSync(
      join(dir, "target-handoff.json"),
      targetHandoffPayload(prepared, inflight, dashboardId),
      { mode: 0o600 },
    );
    fsyncDirectory(dir);
    fs.renameSync(inflight.file, this.paths.dbPath);
    fs.renameSync(join(dir, "ssh_ed25519.key"), this.paths.keyPath);
    fs.renameSync(join(dir, "authorized_keys.roost"), this.paths.authorizedKeysPath);
    fs.renameSync(join(dir, "target-handoff.json"), this.paths.handoffPath);
    fsyncDirectory(dirname(this.paths.dbPath));
    await runInstaller.call(this, "install", inflight.handoffId);
    await assertRelocatedCoordReachable.call(this, prepared.targetUrl);
  } catch (error) {
    if (this.inflight === inflight) {
      try {
        fs.closeSync(inflight.fd);
      } catch {
        // The final chunk already closed this descriptor.
      }
      this.inflight = null;
    }
    try {
      await this.abort(inflight.handoffId);
    } catch (rollbackError) {
      throw new Error(
        `${(error as Error).message}; target rollback failed: ${(rollbackError as Error).message}`,
      );
    }
    throw error;
  }
}
