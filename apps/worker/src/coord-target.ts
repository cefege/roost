// TARGET side of a coordinator relocation: this process is the box ABOUT to
// take the coordinator role. It stages the incoming database/keys into a
// handoff directory, validates everything (checksum, SQLite integrity, key
// kid), and only then swaps files into place and (re)installs the service —
// with a rollback copy of every replaced artifact so any failure unwinds.
// Windows choreography is delegated to coord-relocation-windows.ts; POSIX
// install/rollback runs through install.sh here.
import * as fs from "node:fs";
import { dirname, join } from "node:path";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { resolveTailnetDnsName } from "@roost/shared/tailnet";
import { Database } from "bun:sqlite";
import { COORD_INSTALL_SH } from "@roost/shared/install-scripts";
import { coordServiceLabel, roostServiceDir } from "@roost/shared/paths";
import { durableRemove, durableWriteFile, flushDurablePath } from "@roost/shared/durability";
import { log } from "@roost/shared/log";
import { ROOST_BUILD_SHA } from "@roost/shared/build-identity";
import type { WindowsCoordinatorPromotionRelocationOperation } from "@roost/shared/windows-relocation";
import {
  WindowsCoordinatorTargetRelocation,
} from "./coord-relocation-windows.ts";
import {
  createDefaultWindowsCoordRuntime,
  type WindowsCoordRuntime,
} from "./coord-relocation-windows-runtime.ts";

import {
  RELOCATION_PROBE_TIMEOUT_MS,
  RELOCATION_INSTALLER_TIMEOUT_MS,
  RelocationSpawnTimeoutError,
  restoreServeConfig as runRestoreServeConfig,
  captureServeConfig as runCaptureServeConfig,
  assertCanFrontCoordinator as probeCanFrontCoordinator,
  settleWithinTimeout,
} from "./coord-target-spawns.ts";

const coordLabel = (): string => coordServiceLabel();

export interface CoordTargetPaths {
  dataDir: string;
  dbPath: string;
  keyPath: string;
  authorizedKeysPath: string;
  handoffPath: string;
  /** launchd plist, systemd unit, or Windows service-definition asset. */
  servicePath: string;
}

interface InflightSnapshot {
  handoffId: string;
  file: string;
  fd: number;
  expectedSize: number;
  expectedSha256: string;
  nextSeq: number;
  received: number;
  hasher: Bun.CryptoHasher;
}

interface PreparedTarget {
  handoffId: string;
  sourceUrl: string;
  targetUrl: string;
  expectedCoordKid: string;
  expectedGitSha: string;
  windowsRelocation?: WindowsCoordinatorPromotionRelocationOperation;
}

interface RollbackPresent {
  db: boolean;
  key: boolean;
  authorizedKeys: boolean;
  handoff: boolean;
  service: boolean;
}

export interface CoordTargetRuntime {
  platform: string;
  gitSha: string;
  tailnetDnsName(): string;
  isCoordServiceActive(label: string): Promise<boolean>;
  restoreCoordService(label: string, servicePath: string): Promise<void>;
  /** One reachability probe of the relocated coordinator's advertised URL.
   *  Injectable so tests need no listening socket. */
  coordHealthy?(targetUrl: string): Promise<boolean>;
  windows?: WindowsCoordRuntime;
}

function systemdEnv(): Record<string, string | undefined> {
  return { ...process.env, XDG_RUNTIME_DIR: process.env.XDG_RUNTIME_DIR ?? `/run/user/${process.getuid?.() ?? 0}` };
}

/** MiscHealth is a public Connect endpoint, so no JWT is needed. */
async function defaultCoordHealthy(targetUrl: string): Promise<boolean> {
  const response = await fetch(`${targetUrl.replace(/\/$/, "")}/roost.v1.CoordinatorService/MiscHealth`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: "{}",
  }).catch(() => null);
  return response?.ok === true;
}

const defaultRuntime: CoordTargetRuntime = {
  platform: process.platform,
  gitSha: ROOST_BUILD_SHA !== "dev"
    ? ROOST_BUILD_SHA
    : process.env.GIT_SHA ?? process.env.ROOST_GIT_SHA ?? "dev",
  tailnetDnsName: resolveTailnetDnsName,
  async isCoordServiceActive(label) {
    switch (process.platform) {
      case "darwin": {
        const child = Bun.spawn(["launchctl", "print", `gui/${process.getuid?.() ?? 0}/${label}`], { stdout: "ignore", stderr: "ignore" });
        return await child.exited === 0;
      }
      case "linux": {
        const child = Bun.spawn(["systemctl", "--user", "is-active", `${label}.service`], {
          stdout: "ignore", stderr: "ignore", env: systemdEnv(),
        });
        return await child.exited === 0;
      }
      case "win32":
        return (await createDefaultWindowsCoordRuntime().queryService("coordinator")).state === "running";
      default:
        throw new Error(`unsupported coordinator target platform: ${process.platform}`);
    }
  },
  async restoreCoordService(label, servicePath) {
    switch (process.platform) {
      case "darwin": {
        const uid = process.getuid?.() ?? 0;
        const bootout = Bun.spawn(["launchctl", "bootout", `gui/${uid}/${label}`], { stdout: "ignore", stderr: "ignore" });
        await bootout.exited;
        const bootstrap = Bun.spawn(["launchctl", "bootstrap", `gui/${uid}`, servicePath], { stdout: "ignore", stderr: "ignore" });
        if (await bootstrap.exited !== 0) throw new Error("failed to restore prior coordinator LaunchAgent");
        return;
      }
      case "linux": {
        const reload = Bun.spawn(["systemctl", "--user", "daemon-reload"], { stdout: "ignore", stderr: "ignore", env: systemdEnv() });
        await reload.exited;
        const restart = Bun.spawn(["systemctl", "--user", "restart", `${label}.service`], { stdout: "ignore", stderr: "ignore", env: systemdEnv() });
        if (await restart.exited !== 0) throw new Error("failed to restore prior coordinator systemd unit");
        return;
      }
      case "win32":
        throw new Error("Windows coordinator restoration must use the relocation service transaction");
      default:
        throw new Error(`unsupported coordinator target platform: ${process.platform}`);
    }
  },
  windows: process.platform === "win32" ? createDefaultWindowsCoordRuntime() : undefined,
};

async function coordinatorKeyFingerprint(pem: Uint8Array): Promise<string> {
  const encoded = Buffer.from(pem).toString("utf8").replace(/-----[^-]+-----/g, "").replace(/\s+/g, "");
  const raw = Buffer.from(encoded, "base64");
  const magic = Buffer.from("openssh-key-v1\0", "ascii");
  if (!raw.subarray(0, magic.length).equals(magic)) throw new Error("coordinator key is not an OpenSSH key");
  let position = magic.length;
  const readU32 = (): number => {
    const value = raw.readUInt32BE(position);
    position += 4;
    return value;
  };
  const readString = (): Buffer => {
    const length = readU32();
    const value = raw.subarray(position, position + length);
    position += length;
    return value;
  };
  readString(); readString(); readString(); readU32(); readString();
  const privateBlock = readString();
  let privatePosition = 8; // OpenSSH checkints.
  const readPrivateString = (): Buffer => {
    const length = privateBlock.readUInt32BE(privatePosition);
    privatePosition += 4;
    const value = privateBlock.subarray(privatePosition, privatePosition + length);
    privatePosition += length;
    return value;
  };
  if (readPrivateString().toString("utf8") !== "ssh-ed25519") throw new Error("coordinator key is not ed25519");
  const publicKey = readPrivateString();
  if (publicKey.length !== 32) throw new Error("coordinator key has an invalid public key");
  // The digest MUST come from @roost/shared/fingerprint: worker/coord/web all
  // derive one byte-identical kid from the same raw 32-byte key, and this
  // local createHash copy was exactly the fork that invariant forbids. The
  // PEM/OpenSSH unwrapping above stays local — it parses a PRIVATE key file
  // only the relocation path ever sees.
  return fingerprintOf(publicKey);
}

function existingDirectory(path: string): string {
  let candidate = path;
  while (!fs.existsSync(candidate)) {
    const parent = dirname(candidate);
    if (parent === candidate) throw new Error(`no existing directory for ${path}`);
    candidate = parent;
  }
  return candidate;
}

function assertWritableDirectory(path: string, mustExist = false): void {
  if (mustExist && !fs.existsSync(path)) throw new Error(`${path} does not exist`);
  const directory = existingDirectory(path);
  if (!fs.statSync(directory).isDirectory()) throw new Error(`${directory} is not a directory`);
  fs.accessSync(directory, fs.constants.W_OK);
}

/** Target-side staging: no copied state becomes live before full validation. */
export class CoordTarget {
  #inflight: InflightSnapshot | null = null;
  #prepared: PreparedTarget | null = null;
  readonly #windows: WindowsCoordinatorTargetRelocation | null;

  constructor(private readonly paths: CoordTargetPaths, private readonly runtime: CoordTargetRuntime = defaultRuntime) {
    switch (runtime.platform) {
      case "darwin":
      case "linux":
        this.#windows = null;
        break;
      case "win32":
        this.#windows = new WindowsCoordinatorTargetRelocation({
          ...paths,
          servicePath: join(roostServiceDir(), "service-definitions.json"),
          currentManifestPath: join(roostServiceDir(), "current.json"),
        }, runtime.windows ?? createDefaultWindowsCoordRuntime());
        break;
      default:
        throw new Error(`unsupported coordinator target platform: ${runtime.platform}`);
    }
  }

  async recoverTransaction(): Promise<void> {
    switch (this.runtime.platform) {
      case "darwin":
      case "linux":
        return;
      case "win32":
        if (!this.#windows) throw new Error("Windows coordinator relocation runtime is unavailable");
        await this.#windows.recoverActive();
        return;
      default:
        throw new Error(`unsupported coordinator target platform: ${this.runtime.platform}`);
    }
  }

  async prepare(request: {
    handoff_id: string;
    source_url: string;
    target_url: string;
    expected_coord_kid: string;
    action: "CHECK" | "PREPARE";
    estimated_db_size: bigint;
    expected_git_sha: string;
  }): Promise<void> {
    if (this.runtime.platform !== "win32") {
      // A stale in-flight receive (coord crashed, socket dropped) leaves an
      // open fd behind that would make every retry report "was not prepared".
      this.#discardInflight();
    }
    if (this.runtime.gitSha !== request.expected_git_sha) {
      throw new Error(`worker version ${this.runtime.gitSha} does not match coordinator`);
    }
    const target = new URL(request.target_url);
    const localDnsName = this.runtime.tailnetDnsName().toLowerCase().replace(/\.$/, "");
    if (
      target.protocol !== "https:" || target.port !== "4102" || !target.hostname.endsWith(".ts.net") ||
      !localDnsName || target.hostname.toLowerCase() !== localDnsName
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
    if (available < required) throw new Error(`insufficient disk: required ${required}, available ${available}`);
    await this.assertNoActiveCoordinator();
    if (this.runtime.platform !== "win32") await this.assertCanFrontCoordinator();

    if (this.runtime.platform === "win32") {
      if (!this.#windows) throw new Error("Windows coordinator relocation runtime is unavailable");
      const windowsRelocation = await this.#windows.createOperation(
        request.handoff_id,
        request.source_url,
        request.target_url,
        request.expected_git_sha,
      );
      if (request.action === "CHECK") return;
      this.#discardInflight();
      this.#prepared = {
        handoffId: request.handoff_id,
        sourceUrl: request.source_url,
        targetUrl: request.target_url,
        expectedCoordKid: request.expected_coord_kid,
        expectedGitSha: request.expected_git_sha,
        windowsRelocation,
      };
      await this.#windows.admit(windowsRelocation);
      const handoffDir = join(roostServiceDir(), "data", "worker", "relocation", request.handoff_id);
      await durableWriteFile(join(handoffDir, "prepared.json"), JSON.stringify(this.#prepared), {
        platform: "win32", mode: 0o600, privateDacl: false,
      });
      return;
    }
    if (request.action === "CHECK") return;

    fs.mkdirSync(this.paths.dataDir, { recursive: true, mode: 0o700 });
    const handoffDir = join(this.paths.dataDir, "handoffs", request.handoff_id);
    fs.mkdirSync(handoffDir, { recursive: true, mode: 0o700 });
    this.#prepared = {
      handoffId: request.handoff_id,
      sourceUrl: request.source_url,
      targetUrl: request.target_url,
      expectedCoordKid: request.expected_coord_kid,
      expectedGitSha: request.expected_git_sha,
    };
    // Preserve the pre-move landing before write-plist can replace it.
    this.captureRollback(join(handoffDir, "rollback"));
    await this.captureServeConfig(join(handoffDir, "rollback"));
    await this.stopRetiredCoordinator(request.handoff_id);
    await this.runInstaller("write-plist", request.handoff_id);
    if (!process.env.ROOST_EXEC_BIN) await this.buildSourceSpa();
    fs.writeFileSync(join(handoffDir, "prepared.json"), JSON.stringify(this.#prepared), { mode: 0o600 });
    this.fsyncDirectory(handoffDir);
  }

  /** Reloads the PREPARE record written above when this process restarted
   *  between PREPARE and the snapshot. */
  #loadPrepared(handoffId: string): PreparedTarget | null {
    if (this.#prepared?.handoffId === handoffId) return this.#prepared;
    try {
      const handoffDir = this.runtime.platform === "win32"
        ? join(roostServiceDir(), "data", "worker", "relocation", handoffId)
        : join(this.paths.dataDir, "handoffs", handoffId);
      const parsed = JSON.parse(fs.readFileSync(join(handoffDir, "prepared.json"), "utf8")) as PreparedTarget;
      if (parsed.handoffId !== handoffId) return null;
      this.#prepared = parsed;
      return parsed;
    } catch {
      return null;
    }
  }

  #discardInflight(): void {
    const inflight = this.#inflight;
    if (!inflight) return;
    this.#inflight = null;
    try { fs.closeSync(inflight.fd); } catch { /* already closed */ }
    fs.rmSync(inflight.file, { force: true });
  }

  startSnapshot(request: {
    request_id: string;
    handoff_id: string;
    total_size: bigint;
    sha256: string;
    coord_key_pem: Uint8Array;
    authorized_keys: Uint8Array;
    secret_sha256: string;
    expected_worker_fps: string[];
  }): void | Promise<void> {
    if (this.runtime.platform === "win32") return this.#startWindowsSnapshot(request);
    if (this.runtime.platform !== "darwin" && this.runtime.platform !== "linux") {
      throw new Error(`unsupported coordinator target platform: ${this.runtime.platform}`);
    }
    const prepared = this.#loadPrepared(request.handoff_id);
    if (!prepared) throw new Error("coordinator target was not prepared");
    if (this.#inflight) {
      // A retried START (our rpc-ok was lost) must restart this handoff's
      // receive, not be rejected as "not prepared" — the source never
      // re-sends PREPARE from COPYING_STATE.
      if (this.#inflight.handoffId !== request.handoff_id) {
        throw new Error("another coordinator snapshot is already in flight");
      }
      this.#discardInflight();
    }
    const dir = join(this.paths.dataDir, "handoffs", request.handoff_id);
    const file = join(dir, "coordinator_v2.snapshot");
    fs.rmSync(file, { force: true });
    fs.writeFileSync(join(dir, "ssh_ed25519.key"), request.coord_key_pem, { mode: 0o600 });
    fs.writeFileSync(join(dir, "authorized_keys.roost"), request.authorized_keys, { mode: 0o600 });
    fs.writeFileSync(join(dir, "target-handoff.json"), JSON.stringify({
      version: 1, handoff_id: request.handoff_id, role: "TARGET", phase: "WAITING_FOR_WORKERS",
      source_url: prepared.sourceUrl, target_url: prepared.targetUrl, target_worker_fp: "target-pending",
      expected_worker_fps: request.expected_worker_fps, commit_acked_worker_fps: [],
      expected_coord_kid: prepared.expectedCoordKid, expected_git_sha: prepared.expectedGitSha,
      secret_sha256: request.secret_sha256, started_at_ms: Date.now(), updated_at_ms: Date.now(),
    }), { mode: 0o600 });
    this.fsyncDirectory(dir);
    this.#inflight = {
      handoffId: request.handoff_id, file, fd: fs.openSync(file, "wx", 0o600),
      expectedSize: Number(request.total_size), expectedSha256: request.sha256,
      nextSeq: 0, received: 0, hasher: new Bun.CryptoHasher("sha256"),
    };
  }

  async #startWindowsSnapshot(request: Parameters<CoordTarget["startSnapshot"]>[0]): Promise<void> {
    if (!this.#windows) throw new Error("Windows coordinator relocation runtime is unavailable");
    const prepared = this.#loadPrepared(request.handoff_id);
    if (!prepared?.windowsRelocation) throw new Error("Windows coordinator target was not prepared");
    try {
      if (this.#inflight) {
        if (this.#inflight.handoffId !== request.handoff_id) {
          throw new Error("another coordinator snapshot is already in flight");
        }
        const previous = this.#inflight;
        this.#inflight = null;
        try { fs.closeSync(previous.fd); } catch { /* already closed */ }
        if (fs.existsSync(previous.file)) {
          await durableRemove(previous.file, { platform: "win32", privateDacl: true });
        }
      }
      const dir = join(roostServiceDir(), "data", "worker", "relocation", request.handoff_id);
      const file = join(dir, "coordinator_v2.snapshot");
      if (fs.existsSync(file)) {
        await durableRemove(file, { platform: "win32", privateDacl: true });
      }
      const key = join(dir, "ssh_ed25519.key");
      const authorizedKeys = join(dir, "authorized_keys.roost");
      const handoff = join(dir, "target-handoff.json");
      await durableWriteFile(key, request.coord_key_pem, { platform: "win32", mode: 0o600, privateDacl: false });
      await durableWriteFile(authorizedKeys, request.authorized_keys, { platform: "win32", mode: 0o600, privateDacl: false });
      await durableWriteFile(handoff, JSON.stringify({
        version: 1, handoff_id: request.handoff_id, role: "TARGET", phase: "WAITING_FOR_WORKERS",
        source_url: prepared.sourceUrl, target_url: prepared.targetUrl, target_worker_fp: "target-pending",
        expected_worker_fps: request.expected_worker_fps, commit_acked_worker_fps: [],
        expected_coord_kid: prepared.expectedCoordKid, expected_git_sha: prepared.expectedGitSha,
        secret_sha256: request.secret_sha256, started_at_ms: Date.now(), updated_at_ms: Date.now(),
      }), { platform: "win32", mode: 0o600, privateDacl: false });
      const fd = fs.openSync(file, "wx", 0o600);
      this.#inflight = {
        handoffId: request.handoff_id, file, fd,
        expectedSize: Number(request.total_size), expectedSha256: request.sha256,
        nextSeq: 0, received: 0, hasher: new Bun.CryptoHasher("sha256"),
      };
    } catch (error) {
      await this.abort(request.handoff_id).catch(() => {});
      throw error;
    }
  }

  async appendSnapshot(chunk: { handoff_id: string; seq: number; data: Uint8Array; last: boolean }): Promise<void> {
    const inflight = this.#inflight;
    if (!inflight || inflight.handoffId !== chunk.handoff_id) throw new Error("unknown coordinator snapshot");
    const dir = dirname(inflight.file);
    try {
      // The transport gives no exactly-once guarantee, so a replayed chunk is
      // routine; only a forward gap means bytes are genuinely missing.
      if (chunk.seq < inflight.nextSeq) return;
      if (chunk.seq !== inflight.nextSeq) {
        throw new Error(`snapshot chunk out of order: expected ${inflight.nextSeq}, got ${chunk.seq}`);
      }
      let offset = 0;
      while (offset < chunk.data.length) {
        const written = fs.writeSync(inflight.fd, chunk.data, offset, chunk.data.length - offset);
        if (written === 0) throw new Error("coordinator snapshot write made no progress");
        offset += written;
      }
      inflight.hasher.update(chunk.data);
      inflight.received += chunk.data.length;
      inflight.nextSeq++;
      if (!chunk.last) return;

      fs.fsyncSync(inflight.fd);
      fs.closeSync(inflight.fd);
      this.#inflight = null;
      if (this.runtime.platform === "win32") {
        await flushDurablePath(inflight.file, { platform: "win32" });
      }
      const actualSha256 = inflight.hasher.digest("hex");
      if (inflight.received !== inflight.expectedSize || actualSha256 !== inflight.expectedSha256) {
        throw new Error("coordinator snapshot checksum mismatch");
      }
      const check = new Database(inflight.file, { readonly: true });
      try {
        const integrity = check.query("PRAGMA integrity_check").get() as { integrity_check?: string } | null;
        if (integrity?.integrity_check !== "ok") throw new Error("coordinator snapshot integrity check failed");
      } finally {
        check.close();
      }
      const prepared = this.#loadPrepared(chunk.handoff_id);
      if (!prepared) throw new Error("coordinator target was not prepared");
      if ((await coordinatorKeyFingerprint(fs.readFileSync(join(dir, "ssh_ed25519.key")))) !== prepared.expectedCoordKid) {
        throw new Error("coordinator key fingerprint does not match source");
      }
      if (this.runtime.platform === "win32") {
        if (!this.#windows || !prepared.windowsRelocation) {
          throw new Error("Windows coordinator relocation admission is unavailable");
        }
        await this.#windows.apply(prepared.windowsRelocation);
        return;
      }
      if (this.runtime.platform !== "darwin" && this.runtime.platform !== "linux") {
        throw new Error(`unsupported coordinator target platform: ${this.runtime.platform}`);
      }
      // A -wal/-shm pair left by whatever database previously lived at this
      // path would be replayed into the one we are about to rename in. The
      // retired coordinator that owned them was stopped at PREPARE.
      for (const suffix of ["-wal", "-shm"]) fs.rmSync(`${this.paths.dbPath}${suffix}`, { force: true });
      fs.renameSync(inflight.file, this.paths.dbPath);
      fs.renameSync(join(dir, "ssh_ed25519.key"), this.paths.keyPath);
      fs.renameSync(join(dir, "authorized_keys.roost"), this.paths.authorizedKeysPath);
      fs.renameSync(join(dir, "target-handoff.json"), this.paths.handoffPath);
      this.fsyncDirectory(dirname(this.paths.dbPath));
      await this.runInstaller("install", inflight.handoffId);
      await this.assertRelocatedCoordReachable(prepared.targetUrl);
    } catch (error) {
      if (this.#inflight === inflight) {
        try { fs.closeSync(inflight.fd); } catch { /* already closed */ }
        this.#inflight = null;
      }
      try {
        // abort() removes the whole handoff directory once the rollback lands.
        await this.abort(inflight.handoffId);
      } catch (rollbackError) {
        throw new Error(`${(error as Error).message}; target rollback failed: ${(rollbackError as Error).message}`);
      }
      throw error;
    }
  }

  async abort(handoffId: string): Promise<void> {
    // Any half-received snapshot is dead once we roll back; leaving the fd open
    // wedges every retry behind "coordinator target was not prepared". Only
    // ours, though: a late or duplicate ABORT for a previous handoff must not
    // destroy an unrelated in-flight receive.
    if (this.runtime.platform === "win32") {
      if (!this.#windows) throw new Error("Windows coordinator relocation runtime is unavailable");
      const prepared = this.#loadPrepared(handoffId);
      if (this.#inflight?.handoffId === handoffId) {
        const inflight = this.#inflight;
        this.#inflight = null;
        try { fs.closeSync(inflight.fd); } catch { /* already closed */ }
        if (fs.existsSync(inflight.file)) {
          await durableRemove(inflight.file, { platform: "win32", privateDacl: true });
        }
      }
      if (prepared?.windowsRelocation) await this.#windows.rollback(prepared.windowsRelocation);
      if (this.#prepared?.handoffId === handoffId) this.#prepared = null;
      fs.rmSync(join(roostServiceDir(), "data", "worker", "relocation", handoffId), { recursive: true, force: true });
      return;
    }
    if (this.runtime.platform !== "darwin" && this.runtime.platform !== "linux") {
      throw new Error(`unsupported coordinator target platform: ${this.runtime.platform}`);
    }
    if (this.#inflight?.handoffId === handoffId) this.#discardInflight();
    // The PREPARE record is about to be deleted with the directory below; a
    // stale in-memory copy would let a late START open a file under a
    // directory that no longer exists instead of reporting "not prepared".
    if (this.#prepared?.handoffId === handoffId) this.#prepared = null;
    const handoffDir = join(this.paths.dataDir, "handoffs", handoffId);
    const rollback = join(handoffDir, "rollback");
    let present: RollbackPresent;
    try { present = JSON.parse(fs.readFileSync(join(rollback, "present.json"), "utf8")) as RollbackPresent; } catch { return; }
    // Uninstall first: install.sh's `uninstall` only stops the service and
    // removes the unit (it leaves DB + keys alone), and a coordinator still
    // holding dbPath open would be corrupted by copying over it. Non-fatal
    // though — a throwing uninstall must not strand an unrestored filesystem.
    try {
      await this.runInstaller("uninstall", handoffId);
    } catch (error) {
      log.warn("coord-target", "uninstall_failed", { handoff_id: handoffId, error: String(error) });
    }
    for (const [canonical, name, existed] of this.rollbackSlots(present)) {
      const backup = join(rollback, name);
      if (existed) fs.copyFileSync(backup, canonical);
      else fs.rmSync(canonical, { force: true });
    }
    this.fsyncDirectory(dirname(this.paths.dbPath));
    await this.restoreServeConfig(rollback);
    if (present.service) await this.runtime.restoreCoordService(coordLabel(), this.paths.servicePath);
    // startSnapshot stages the SOURCE coordinator's private key in here and
    // nothing else removes it. The rollback has just been applied, so the
    // directory — rollback copies included — is dead weight.
    fs.rmSync(handoffDir, { recursive: true, force: true });
  }

  /** install.sh's `serve_front` only warns when `tailscale serve` fails, and we
   *  spawn it with stdio ignored — so a target that bound loopback but is
   *  unreachable at its advertised URL looks like a clean install. Failing here
   *  rolls back before the source burns its 60s worker drain. */
  private async assertRelocatedCoordReachable(targetUrl: string): Promise<void> {
    const probe = this.runtime.coordHealthy ?? defaultCoordHealthy;
    for (let attempt = 0; attempt < 5; attempt++) {
      // The freshly installed service needs a moment to bind.
      if (attempt > 0) await Bun.sleep(2_000);
      if (await probe(targetUrl)) return;
    }
    throw new Error(`relocated coordinator is not reachable at ${targetUrl}`);
  }

  private rollbackSlots(present: RollbackPresent): readonly (readonly [string, string, boolean])[] {
    return [
      [this.paths.dbPath, "coordinator_v2.db", present.db],
      [this.paths.keyPath, "ssh_ed25519.key", present.key],
      [this.paths.authorizedKeysPath, "authorized_keys.roost", present.authorizedKeys],
      [this.paths.handoffPath, "coord-handoff.json", present.handoff],
      [this.paths.servicePath, "coordinator.service-definition", present.service],
    ] as const;
  }

  private captureRollback(rollback: string): void {
    if (fs.existsSync(join(rollback, "present.json"))) return;
    fs.mkdirSync(rollback, { recursive: true, mode: 0o700 });
    const present: RollbackPresent = {
      db: fs.existsSync(this.paths.dbPath), key: fs.existsSync(this.paths.keyPath),
      authorizedKeys: fs.existsSync(this.paths.authorizedKeysPath), handoff: fs.existsSync(this.paths.handoffPath),
      service: fs.existsSync(this.paths.servicePath),
    };
    for (const [canonical, name, exists] of this.rollbackSlots(present)) {
      if (exists) fs.copyFileSync(canonical, join(rollback, name));
    }
    fs.writeFileSync(join(rollback, "present.json"), JSON.stringify(present), { mode: 0o600 });
    this.fsyncDirectory(rollback);
  }

  private async captureServeConfig(rollback: string): Promise<void> {
    await runCaptureServeConfig(rollback);
    this.fsyncDirectory(rollback);
  }

  private async restoreServeConfig(rollback: string): Promise<void> {
    await runRestoreServeConfig(rollback);
  }

  /** A retired source coordinator is deliberately left running to serve
   *  discovery, so a live coordinator service alone must not block the role
   *  coming back. Only a coordinator that is NOT a committed source does. */
  private async assertNoActiveCoordinator(): Promise<boolean> {
    if (!await this.runtime.isCoordServiceActive(coordLabel())) return false;
    try {
      const local = JSON.parse(fs.readFileSync(this.paths.handoffPath, "utf8")) as { role?: string; phase?: string };
      if (local.role === "SOURCE" && local.phase === "COMMITTED") return true;
    } catch {
      // No readable handoff file: this is a plain active coordinator.
    }
    throw new Error("target already has an active coordinator");
  }
  private async assertCanFrontCoordinator(): Promise<void> {
    await probeCanFrontCoordinator(this.runtime.platform);
  }

  /** Finalizes the updater transaction after the source has retired. */
  async finalizeCommit(handoffId: string): Promise<void> {
    switch (this.runtime.platform) {
      case "darwin":
      case "linux":
        fs.rmSync(join(this.paths.dataDir, "handoffs", handoffId), { recursive: true, force: true });
        return;
      case "win32": {
        if (!this.#windows) throw new Error("Windows coordinator relocation runtime is unavailable");
        const prepared = this.#loadPrepared(handoffId);
        if (!prepared?.windowsRelocation) throw new Error("Windows coordinator relocation was not prepared");
        await this.#windows.commit(prepared.windowsRelocation);
        if (this.#prepared?.handoffId === handoffId) this.#prepared = null;
        return;
      }
      default:
        throw new Error(`unsupported coordinator target platform: ${this.runtime.platform}`);
    }
  }

  /** assertNoActiveCoordinator lets a retired SOURCE box accept a move back,
   *  but that coordinator is still RUNNING and still holds coordinator_v2.db
   *  open in WAL mode. Promotion renames a new database over that exact path,
   *  so the old process would go on writing a -wal belonging to an inode that
   *  no longer exists — silent corruption of the DB that just arrived.
   *  Unlinking -wal at promotion is not enough on its own: a live process
   *  recreates it immediately. The process has to go first. */
  private async stopRetiredCoordinator(handoffId: string): Promise<void> {
    if (!await this.runtime.isCoordServiceActive(coordLabel())) return;
    // A live retired coordinator can recreate WAL files after promotion and
    // corrupt the database moved over the same path. Failure to stop it is a
    // hard precondition failure, not something unlinking -wal can repair.
    await this.runInstaller("uninstall", handoffId);
    log.info("coord-target", "retired_coordinator_stopped", { handoff_id: handoffId });
  }

  private async buildSourceSpa(): Promise<void> {
    const webDir = join(process.cwd(), "apps", "web");
    const child = Bun.spawn(["bun", "run", "build"], {
      cwd: webDir,
      env: { ...process.env, ROOST_GIT_SHA: this.runtime.gitSha },
      stdout: "pipe",
      stderr: "pipe",
    });
    // Both pipes must be drained: vite is chatty on stdout and an unread pipe
    // buffer blocks the child, hanging PREPARE.
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
      throw new Error(`target web build failed in ${webDir} (exit ${code})${detail ? `: ${detail}` : ""}`);
    }
  }

  private async runInstaller(command: "write-plist" | "install" | "uninstall", handoffId: string): Promise<void> {
    if (this.runtime.platform === "win32") {
      throw new Error("Windows coordinator promotion must use the SCM relocation transaction");
    }
    if (this.runtime.platform !== "darwin" && this.runtime.platform !== "linux") {
      throw new Error(`unsupported coordinator target platform: ${this.runtime.platform}`);
    }
    const dir = join(this.paths.dataDir, "handoffs", handoffId);
    fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
    const script = process.env.ROOST_COORDINATOR_INSTALL_SCRIPT ?? (COORD_INSTALL_SH ? join(dir, "install-coordinator.sh") : join(process.cwd(), "apps", "coord", "scripts", "install.sh"));
    if (COORD_INSTALL_SH && !process.env.ROOST_COORDINATOR_INSTALL_SCRIPT) fs.writeFileSync(script, COORD_INSTALL_SH, { mode: 0o700 });
    const child = Bun.spawn(["bash", script, command], {
      env: {
        ...process.env, ROOST_REPO_ROOT: process.cwd(), ROOST_COORD_DATA_DIR: this.paths.dataDir,
        ROOST_COORDINATOR_DB: this.paths.dbPath, ROOST_COORDINATOR_KEY_PATH: this.paths.keyPath,
        ROOST_COORDINATOR_AUTHORIZED_KEYS: this.paths.authorizedKeysPath, ROOST_COORDINATOR_HANDOFF_PATH: this.paths.handoffPath,
        ROOST_COORDINATOR_PUBLIC_URL: this.#prepared?.targetUrl ?? process.env.ROOST_COORDINATOR_PUBLIC_URL,
        ROOST_COORD_PLIST: this.paths.servicePath,
        ROOST_COORD_UNIT: this.paths.servicePath,
        // The deployed tree has no .git, so install.sh's `git rev-parse` fallback
        // yields nothing and the relocated coord reports "dev" — which lights the
        // drift badge on every worker and makes preflight refuse the move back.
        ROOST_GIT_SHA: this.runtime.gitSha,
        // Compiled-binary deploys: without this install.sh takes the
        // from-source branch and writes ExecStart=<bun> $HOME/apps/coord/src/main.ts,
        // which crash-loops under KeepAlive.
        ROOST_EXEC_BIN: process.env.ROOST_EXEC_BIN,
        // The deployed repo's .env.local normally carries the SOURCE
        // coordinator's public URL; sourcing it would make the new coordinator
        // advertise the old host.
        ROOST_SKIP_ENV_LOCAL: "1",
      },
      // Captured, not ignored: every installer failure used to surface as a
      // bare "coordinator <cmd> failed" with the child's actual diagnosis
      // thrown away, and this error text is what the SPA shows the operator.
      stdout: "pipe", stderr: "pipe",
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
      throw new Error(`coordinator ${command} failed (exit ${code})${detail ? `: ${detail}` : ` running ${script}`}`);
    }
  }

  private fsyncDirectory(path: string): void {
    const fd = fs.openSync(path, "r");
    try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
  }
}
