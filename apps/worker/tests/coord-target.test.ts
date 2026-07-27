import { afterEach, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import * as fs from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { Database } from "bun:sqlite";
import { loadOrCreateCoordKey } from "../../coord/src/coord-key.ts";
import { CoordTarget, type CoordTargetPaths } from "../src/coord-target.ts";

const roots: string[] = [];
afterEach(() => roots.splice(0).forEach((root) => fs.rmSync(root, { recursive: true, force: true })));

test("verified bundle preserves coordinator key identity and abort restores canonical state and Serve", async () => {
  const root = fs.mkdtempSync(join(tmpdir(), "roost-coord-target-"));
  roots.push(root);
  const bin = join(root, "bin");
  const data = join(root, "data");
  fs.mkdirSync(bin, { recursive: true });
  const serveLog = join(root, "serve.log");
  const tailscale = join(bin, "tailscale");
  fs.writeFileSync(tailscale, `#!/bin/bash\nif [[ "$2" == "get-config" ]]; then echo '{"TCP":{}}' > "$3"; fi\necho "$@" >> "${serveLog}"\n`);
  fs.chmodSync(tailscale, 0o700);
  const installer = join(bin, "install.sh");
  fs.writeFileSync(installer, "#!/bin/bash\necho \"$1\" >> \"$ROOST_TEST_INSTALL_LOG\"\n");
  fs.chmodSync(installer, 0o700);
  const old = { db: "old-db", key: "old-key", auth: "old-auth", handoff: "old-handoff", plist: "old-plist" };
  const paths = { dataDir: data, dbPath: join(data, "coordinator_v2.db"), keyPath: join(data, "ssh_ed25519.key"), authorizedKeysPath: join(data, "authorized_keys.roost"), handoffPath: join(data, "coord-handoff.json"), servicePath: join(root, "coord.plist") };
  fs.mkdirSync(data, { recursive: true });
  fs.writeFileSync(paths.dbPath, old.db); fs.writeFileSync(paths.keyPath, old.key); fs.writeFileSync(paths.authorizedKeysPath, old.auth); fs.writeFileSync(paths.handoffPath, old.handoff); fs.writeFileSync(paths.servicePath, old.plist);
  const sourceKeyPath = join(root, "source.key");
  const sourceKey = await loadOrCreateCoordKey(sourceKeyPath);
  const snapshotPath = join(root, "snapshot.db");
  const snapshotDb = new Database(snapshotPath); snapshotDb.exec("CREATE TABLE proof (value TEXT); INSERT INTO proof VALUES ('copy');"); snapshotDb.close();
  const bytes = fs.readFileSync(snapshotPath);
  const handoff = "00000000-0000-4000-8000-000000000001";
  const previous = { path: process.env.PATH, exec: process.env.ROOST_EXEC_BIN, install: process.env.ROOST_COORDINATOR_INSTALL_SCRIPT, installLog: process.env.ROOST_TEST_INSTALL_LOG, tailscale: process.env.ROOST_TAILSCALE_BIN };
  process.env.ROOST_EXEC_BIN = "roost"; process.env.ROOST_COORDINATOR_INSTALL_SCRIPT = installer; process.env.ROOST_TEST_INSTALL_LOG = join(root, "install.log"); process.env.ROOST_TAILSCALE_BIN = tailscale;
  try {
    const target = new CoordTarget(paths, { platform: "darwin", gitSha: "sha", tailnetDnsName: () => "target.ts.net", isCoordServiceActive: async () => false, restoreCoordService: async () => {}, coordHealthy: async () => true });
    await target.prepare({ handoff_id: handoff, source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102", expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: BigInt(bytes.length), action: "PREPARE" });
    target.startSnapshot({ request_id: "request", handoff_id: handoff, total_size: BigInt(bytes.length), sha256: createHash("sha256").update(bytes).digest("hex"), coord_key_pem: fs.readFileSync(sourceKeyPath), authorized_keys: Buffer.from("new-auth"), secret_sha256: "a".repeat(64), expected_worker_fps: ["worker"] });
    await target.appendSnapshot({ handoff_id: handoff, seq: 0, data: bytes, last: true });
    const copied = new Database(paths.dbPath, { readonly: true }); expect(copied.query("SELECT value FROM proof").get()).toEqual({ value: "copy" }); copied.close();
    expect(fs.readFileSync(paths.keyPath)).toEqual(fs.readFileSync(sourceKeyPath));
    await target.abort(handoff);
    expect(fs.readFileSync(paths.dbPath, "utf8")).toBe(old.db); expect(fs.readFileSync(paths.keyPath, "utf8")).toBe(old.key); expect(fs.readFileSync(paths.authorizedKeysPath, "utf8")).toBe(old.auth); expect(fs.readFileSync(paths.handoffPath, "utf8")).toBe(old.handoff); expect(fs.readFileSync(paths.servicePath, "utf8")).toBe(old.plist);
    expect(fs.readFileSync(serveLog, "utf8")).toContain("set-config");
    const failedHandoff = "00000000-0000-4000-8000-000000000002";
    await target.prepare({ handoff_id: failedHandoff, source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102", expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: BigInt(bytes.length), action: "PREPARE" });
    target.startSnapshot({ request_id: "bad-request", handoff_id: failedHandoff, total_size: BigInt(bytes.length), sha256: "0".repeat(64), coord_key_pem: fs.readFileSync(sourceKeyPath), authorized_keys: Buffer.from("new-auth"), secret_sha256: "a".repeat(64), expected_worker_fps: ["worker"] });
    await expect(target.appendSnapshot({ handoff_id: failedHandoff, seq: 0, data: bytes, last: true })).rejects.toThrow("coordinator snapshot checksum mismatch");
    expect(fs.readFileSync(paths.dbPath, "utf8")).toBe(old.db);
    expect(fs.readFileSync(paths.keyPath, "utf8")).toBe(old.key);
    expect(fs.readFileSync(paths.authorizedKeysPath, "utf8")).toBe(old.auth);
    expect(fs.readFileSync(paths.handoffPath, "utf8")).toBe(old.handoff);
    // The failed append above rolled the handoff back and removed its staging
    // directory, so the out-of-order case needs a fresh PREPARE.
    await target.prepare({ handoff_id: failedHandoff, source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102", expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: BigInt(bytes.length), action: "PREPARE" });
    target.startSnapshot({ request_id: "out-of-order-request", handoff_id: failedHandoff, total_size: BigInt(bytes.length), sha256: createHash("sha256").update(bytes).digest("hex"), coord_key_pem: fs.readFileSync(sourceKeyPath), authorized_keys: Buffer.from("new-auth"), secret_sha256: "a".repeat(64), expected_worker_fps: ["worker"] });
    await expect(target.appendSnapshot({ handoff_id: failedHandoff, seq: 1, data: bytes, last: false })).rejects.toThrow("snapshot chunk out of order");
    expect(fs.readFileSync(paths.dbPath, "utf8")).toBe(old.db);
    expect(fs.readFileSync(paths.keyPath, "utf8")).toBe(old.key);
    const freshData = join(root, "fresh-coord-data");
    const freshPaths = {
      dataDir: freshData,
      dbPath: join(freshData, "coordinator_v2.db"),
      keyPath: join(freshData, "ssh_ed25519.key"),
      authorizedKeysPath: join(freshData, "authorized_keys.roost"),
      handoffPath: join(freshData, "coord-handoff.json"),
      servicePath: join(root, "fresh-coord.plist"),
    };
    const freshTarget = new CoordTarget(freshPaths, { platform: "darwin", gitSha: "sha", tailnetDnsName: () => "target.ts.net", isCoordServiceActive: async () => false, restoreCoordService: async () => {}, coordHealthy: async () => true });
    await freshTarget.prepare({ handoff_id: "00000000-0000-4000-8000-000000000003", source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102", expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: BigInt(bytes.length), action: "CHECK" });
    expect(fs.existsSync(freshData)).toBeFalse();
    await freshTarget.prepare({ handoff_id: "00000000-0000-4000-8000-000000000003", source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102", expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: BigInt(bytes.length), action: "PREPARE" });
    expect(fs.existsSync(freshData)).toBeTrue();

    await expect(new CoordTarget(paths, {
      platform: "darwin", gitSha: "stale", tailnetDnsName: () => "target.ts.net",
      isCoordServiceActive: async () => false, restoreCoordService: async () => {}, coordHealthy: async () => true,
    }).prepare({
      handoff_id: handoff, source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102",
      expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: 0n, action: "CHECK",
    })).rejects.toThrow("worker version stale does not match coordinator");

    await expect(new CoordTarget(paths, {
      platform: "darwin", gitSha: "sha", tailnetDnsName: () => "target.ts.net",
      isCoordServiceActive: async () => false, restoreCoordService: async () => {}, coordHealthy: async () => true,
    }).prepare({
      handoff_id: handoff, source_url: "https://source.example:4102", target_url: "https://other.ts.net:4102",
      expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: 0n, action: "CHECK",
    })).rejects.toThrow("target URL does not match this worker's Tailscale address");

    await expect(new CoordTarget(paths, {
      platform: "darwin", gitSha: "sha", tailnetDnsName: () => "target.ts.net",
      isCoordServiceActive: async () => false, restoreCoordService: async () => {}, coordHealthy: async () => true,
    }).prepare({
      handoff_id: handoff, source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102",
      expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha",
      estimated_db_size: BigInt(Number.MAX_SAFE_INTEGER), action: "CHECK",
    })).rejects.toThrow("insufficient disk");
  } finally {
    const restore = (name: string, value: string | undefined): void => {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    };
    restore("PATH", previous.path);
    restore("ROOST_EXEC_BIN", previous.exec);
    restore("ROOST_COORDINATOR_INSTALL_SCRIPT", previous.install);
    restore("ROOST_TEST_INSTALL_LOG", previous.installLog);
    restore("ROOST_TAILSCALE_BIN", previous.tailscale);
  }
});

/** Shared rig for the frame-idempotency cases below: stub installer + stub
 *  tailscale so PREPARE/ABORT never touch the real service or the tailnet, and
 *  ROOST_EXEC_BIN set so PREPARE skips the SPA build. */
const savedEnv: [string, string | undefined][] = [];
afterEach(() => {
  for (const [name, value] of savedEnv.splice(0)) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
});

interface TargetHarness {
  paths: CoordTargetPaths;
  target: CoordTarget;
  bytes: Buffer;
  sha256: string;
  old: { db: string; key: string };
  prepare(handoffId: string): Promise<void>;
  start(handoffId: string): void;
  handoffDir(handoffId: string): string;
  staged(handoffId: string): string;
}

async function harness(): Promise<TargetHarness> {
  const root = fs.mkdtempSync(join(tmpdir(), "roost-coord-target-"));
  roots.push(root);
  const bin = join(root, "bin");
  const data = join(root, "data");
  fs.mkdirSync(bin, { recursive: true });
  const tailscale = join(bin, "tailscale");
  fs.writeFileSync(tailscale, `#!/bin/bash\nif [[ "$2" == "get-config" ]]; then echo '{"TCP":{}}' > "$3"; fi\n`);
  fs.chmodSync(tailscale, 0o700);
  const installer = join(bin, "install.sh");
  fs.writeFileSync(installer, "#!/bin/bash\nexit 0\n");
  fs.chmodSync(installer, 0o700);
  for (const name of ["ROOST_EXEC_BIN", "ROOST_COORDINATOR_INSTALL_SCRIPT", "ROOST_TAILSCALE_BIN"]) {
    savedEnv.push([name, process.env[name]]);
  }
  process.env.ROOST_EXEC_BIN = "roost";
  process.env.ROOST_COORDINATOR_INSTALL_SCRIPT = installer;
  process.env.ROOST_TAILSCALE_BIN = tailscale;

  const old = { db: "old-db", key: "old-key" };
  const paths: CoordTargetPaths = {
    dataDir: data,
    dbPath: join(data, "coordinator_v2.db"),
    keyPath: join(data, "ssh_ed25519.key"),
    authorizedKeysPath: join(data, "authorized_keys.roost"),
    handoffPath: join(data, "coord-handoff.json"),
    servicePath: join(root, "coord.plist"),
  };
  fs.mkdirSync(data, { recursive: true });
  fs.writeFileSync(paths.dbPath, old.db);
  fs.writeFileSync(paths.keyPath, old.key);
  fs.writeFileSync(paths.authorizedKeysPath, "old-auth");
  fs.writeFileSync(paths.handoffPath, "old-handoff");
  fs.writeFileSync(paths.servicePath, "old-plist");
  const sourceKeyPath = join(root, "source.key");
  const sourceKey = await loadOrCreateCoordKey(sourceKeyPath);
  const keyPem = fs.readFileSync(sourceKeyPath);
  const snapshotPath = join(root, "snapshot.db");
  const snapshotDb = new Database(snapshotPath);
  snapshotDb.exec("CREATE TABLE proof (value TEXT); INSERT INTO proof VALUES ('copy');");
  snapshotDb.close();
  const bytes = fs.readFileSync(snapshotPath);
  const sha256 = createHash("sha256").update(bytes).digest("hex");
  const target = new CoordTarget(paths, { platform: "darwin", gitSha: "sha", tailnetDnsName: () => "target.ts.net", isCoordServiceActive: async () => false, restoreCoordService: async () => {}, coordHealthy: async () => true });
  const handoffDir = (handoffId: string): string => join(data, "handoffs", handoffId);
  return {
    paths, target, bytes, sha256, old, handoffDir,
    staged: (handoffId) => join(handoffDir(handoffId), "coordinator_v2.snapshot"),
    prepare: (handoffId) => target.prepare({ handoff_id: handoffId, source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102", expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: BigInt(bytes.length), action: "PREPARE" }),
    start: (handoffId) => target.startSnapshot({ request_id: `request-${handoffId}`, handoff_id: handoffId, total_size: BigInt(bytes.length), sha256, coord_key_pem: keyPem, authorized_keys: Buffer.from("new-auth"), secret_sha256: "a".repeat(64), expected_worker_fps: ["worker"] }),
  };
}

/** Asserts the canonical DB is the promoted snapshot, byte for byte. */
function expectPromoted(h: TargetHarness): void {
  expect(createHash("sha256").update(fs.readFileSync(h.paths.dbPath)).digest("hex")).toBe(h.sha256);
  const promoted = new Database(h.paths.dbPath, { readonly: true });
  try {
    expect(promoted.query("SELECT value FROM proof").get()).toEqual({ value: "copy" });
  } finally {
    promoted.close();
  }
}

const HANDOFF_A = "00000000-0000-4000-8000-00000000000a";
const HANDOFF_B = "00000000-0000-4000-8000-00000000000b";

test("a replayed snapshot chunk is ignored instead of aborting the transfer", async () => {
  const h = await harness();
  const mid = Math.floor(h.bytes.length / 2);
  await h.prepare(HANDOFF_A);
  h.start(HANDOFF_A);
  await h.target.appendSnapshot({ handoff_id: HANDOFF_A, seq: 0, data: h.bytes.subarray(0, mid), last: false });
  // The transport has no exactly-once guarantee: a repeat of an applied chunk
  // is a replay, not corruption, and must neither throw nor be counted twice.
  await h.target.appendSnapshot({ handoff_id: HANDOFF_A, seq: 0, data: h.bytes.subarray(0, mid), last: false });
  expect(fs.statSync(h.staged(HANDOFF_A)).size).toBe(mid);
  await h.target.appendSnapshot({ handoff_id: HANDOFF_A, seq: 1, data: h.bytes.subarray(mid), last: true });
  expectPromoted(h);
});

test("abort of a previous handoff leaves an unrelated in-flight snapshot intact", async () => {
  const h = await harness();
  const mid = Math.floor(h.bytes.length / 2);
  // PREPARE discards any in-flight receive, so B has to be prepared first.
  await h.prepare(HANDOFF_B);
  await h.prepare(HANDOFF_A);
  h.start(HANDOFF_A);
  await h.target.appendSnapshot({ handoff_id: HANDOFF_A, seq: 0, data: h.bytes.subarray(0, mid), last: false });
  await h.target.abort(HANDOFF_B);
  expect(fs.statSync(h.staged(HANDOFF_A)).size).toBe(mid);
  await h.target.appendSnapshot({ handoff_id: HANDOFF_A, seq: 1, data: h.bytes.subarray(mid), last: true });
  expectPromoted(h);
});

test("abort removes the staged source coordinator private key", async () => {
  const h = await harness();
  await h.prepare(HANDOFF_A);
  h.start(HANDOFF_A);
  const stagedKey = join(h.handoffDir(HANDOFF_A), "ssh_ed25519.key");
  expect(fs.existsSync(stagedKey)).toBeTrue();
  await h.target.abort(HANDOFF_A);
  expect(fs.existsSync(stagedKey)).toBeFalse();
  expect(fs.existsSync(h.handoffDir(HANDOFF_A))).toBeFalse();
  expect(fs.readFileSync(h.paths.keyPath, "utf8")).toBe(h.old.key);
});

test("a retried start for the same handoff restarts the receive from seq 0", async () => {
  const h = await harness();
  const mid = Math.floor(h.bytes.length / 2);
  await h.prepare(HANDOFF_A);
  h.start(HANDOFF_A);
  await h.target.appendSnapshot({ handoff_id: HANDOFF_A, seq: 0, data: h.bytes.subarray(0, mid), last: false });
  // The source never re-sends PREPARE from COPYING_STATE, so a START whose
  // rpc-ok was lost must restart this receive rather than be rejected.
  expect(() => h.start(HANDOFF_A)).not.toThrow();
  expect(fs.statSync(h.staged(HANDOFF_A)).size).toBe(0);
  await h.target.appendSnapshot({ handoff_id: HANDOFF_A, seq: 0, data: h.bytes.subarray(0, mid), last: false });
  await h.target.appendSnapshot({ handoff_id: HANDOFF_A, seq: 1, data: h.bytes.subarray(mid), last: true });
  expectPromoted(h);
});

test("a start for a different handoff while one is in flight reports the conflict", async () => {
  const h = await harness();
  const mid = Math.floor(h.bytes.length / 2);
  await h.prepare(HANDOFF_B);
  await h.prepare(HANDOFF_A);
  h.start(HANDOFF_A);
  await h.target.appendSnapshot({ handoff_id: HANDOFF_A, seq: 0, data: h.bytes.subarray(0, mid), last: false });
  expect(() => h.start(HANDOFF_B)).toThrow("another coordinator snapshot is already in flight");
  await h.target.appendSnapshot({ handoff_id: HANDOFF_A, seq: 1, data: h.bytes.subarray(mid), last: true });
  expectPromoted(h);
});

test("promotion discards a -wal/-shm pair left by the database it replaces", async () => {
  // A move back onto a retired source lands on a box whose old coordinator
  // owned coordinator_v2.db in WAL mode. Those sidecars belong to an inode
  // that is about to disappear; SQLite would replay them into the new file.
  const h = await harness();
  fs.writeFileSync(`${h.paths.dbPath}-wal`, "stale-wal-from-the-previous-database");
  fs.writeFileSync(`${h.paths.dbPath}-shm`, "stale-shm");

  await h.prepare(HANDOFF_A);
  h.start(HANDOFF_A);
  await h.target.appendSnapshot({ handoff_id: HANDOFF_A, seq: 0, data: h.bytes, last: true });

  expect(fs.existsSync(`${h.paths.dbPath}-wal`)).toBeFalse();
  expect(fs.existsSync(`${h.paths.dbPath}-shm`)).toBeFalse();
  expectPromoted(h);
});

test("finalizeCommit removes the staged rollback, which holds the coordinator key", async () => {
  // captureRollback copies the pre-move DB and signing key aside so a failed
  // move can be undone. abort() deletes them; before finalizeCommit the commit
  // path never did, so every completed move left another copy on disk.
  const h = await harness();
  await h.prepare(HANDOFF_A);
  const rollbackKey = join(h.handoffDir(HANDOFF_A), "rollback", "ssh_ed25519.key");
  expect(fs.existsSync(rollbackKey)).toBeTrue();

  h.target.finalizeCommit(HANDOFF_A);

  expect(fs.existsSync(rollbackKey)).toBeFalse();
  expect(fs.existsSync(h.handoffDir(HANDOFF_A))).toBeFalse();
  // A worker that never hosted the move has no such directory: must not throw.
  expect(() => h.target.finalizeCommit(HANDOFF_B)).not.toThrow();
});

test("a linux target that cannot configure tailscale serve is rejected at CHECK", async () => {
  // install.sh's serve_front only warns when `tailscale serve` fails, so
  // without this the coordinator installs, binds loopback, and is unreachable
  // at the https URL it advertises — discovered only after the DB is swapped.
  const h = await harness();
  const denied = join(dirname(process.env.ROOST_TAILSCALE_BIN!), "tailscale-denied");
  fs.writeFileSync(denied, "#!/bin/bash\nif [[ \"$1\" == \"serve\" ]]; then echo 'Access denied: serve config denied' >&2; exit 1; fi\nexit 0\n");
  fs.chmodSync(denied, 0o700);
  const previous = process.env.ROOST_TAILSCALE_BIN;
  process.env.ROOST_TAILSCALE_BIN = denied;
  try {
    const linux = new CoordTarget(h.paths, {
      platform: "linux", gitSha: "sha", tailnetDnsName: () => "target.ts.net",
      isCoordServiceActive: async () => false, restoreCoordService: async () => {}, coordHealthy: async () => true,
    });
    await expect(linux.prepare({
      handoff_id: HANDOFF_B, source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102",
      expected_coord_kid: "kid", expected_git_sha: "sha", estimated_db_size: 1n, action: "CHECK",
    })).rejects.toThrow("cannot configure tailscale serve");
  } finally {
    process.env.ROOST_TAILSCALE_BIN = previous;
  }
});

test("the serve capability probe is skipped for a direct-TLS target", async () => {
  // serve_front only runs when FRONTED==1 (install.sh:299); a direct-TLS box
  // never invokes `tailscale serve`, so its capability is irrelevant and must
  // not manufacture a blocker.
  const h = await harness();
  const previous = process.env.ROOST_FRONTED;
  process.env.ROOST_FRONTED = "0";
  try {
    const linux = new CoordTarget(h.paths, {
      platform: "linux", gitSha: "sha", tailnetDnsName: () => "target.ts.net",
      isCoordServiceActive: async () => false, restoreCoordService: async () => {}, coordHealthy: async () => true,
    });
    await linux.prepare({
      handoff_id: HANDOFF_B, source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102",
      expected_coord_kid: "kid", expected_git_sha: "sha", estimated_db_size: 1n, action: "CHECK",
    });
  } finally {
    if (previous === undefined) delete process.env.ROOST_FRONTED;
    else process.env.ROOST_FRONTED = previous;
  }
});
