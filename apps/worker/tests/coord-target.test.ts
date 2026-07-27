import { afterEach, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import * as fs from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Database } from "bun:sqlite";
import { loadOrCreateCoordKey } from "../../coord/src/coord-key.ts";
import { CoordTarget } from "../src/coord-target.ts";

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
  const paths = { dataDir: data, dbPath: join(data, "coordinator_v2.db"), keyPath: join(data, "ssh_ed25519.key"), authorizedKeysPath: join(data, "authorized_keys.roost"), handoffPath: join(data, "coord-handoff.json"), plistPath: join(root, "coord.plist") };
  fs.mkdirSync(data, { recursive: true });
  fs.writeFileSync(paths.dbPath, old.db); fs.writeFileSync(paths.keyPath, old.key); fs.writeFileSync(paths.authorizedKeysPath, old.auth); fs.writeFileSync(paths.handoffPath, old.handoff); fs.writeFileSync(paths.plistPath, old.plist);
  const sourceKeyPath = join(root, "source.key");
  const sourceKey = await loadOrCreateCoordKey(sourceKeyPath);
  const snapshotPath = join(root, "snapshot.db");
  const snapshotDb = new Database(snapshotPath); snapshotDb.exec("CREATE TABLE proof (value TEXT); INSERT INTO proof VALUES ('copy');"); snapshotDb.close();
  const bytes = fs.readFileSync(snapshotPath);
  const handoff = "00000000-0000-4000-8000-000000000001";
  const previous = { path: process.env.PATH, exec: process.env.ROOST_EXEC_BIN, install: process.env.ROOST_COORDINATOR_INSTALL_SCRIPT, installLog: process.env.ROOST_TEST_INSTALL_LOG, tailscale: process.env.ROOST_TAILSCALE_BIN };
  process.env.ROOST_EXEC_BIN = "roost"; process.env.ROOST_COORDINATOR_INSTALL_SCRIPT = installer; process.env.ROOST_TEST_INSTALL_LOG = join(root, "install.log"); process.env.ROOST_TAILSCALE_BIN = tailscale;
  try {
    const target = new CoordTarget(paths, { platform: "darwin", gitSha: "sha", tailnetDnsName: () => "target.ts.net", isLaunchAgentActive: async () => false, restoreLaunchAgent: async () => {} });
    await target.prepare({ handoff_id: handoff, source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102", expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: BigInt(bytes.length), action: "PREPARE" });
    target.startSnapshot({ request_id: "request", handoff_id: handoff, total_size: BigInt(bytes.length), sha256: createHash("sha256").update(bytes).digest("hex"), coord_key_pem: fs.readFileSync(sourceKeyPath), authorized_keys: Buffer.from("new-auth"), secret_sha256: "a".repeat(64), expected_worker_fps: ["worker"] });
    await target.appendSnapshot({ handoff_id: handoff, seq: 0, data: bytes, last: true });
    const copied = new Database(paths.dbPath, { readonly: true }); expect(copied.query("SELECT value FROM proof").get()).toEqual({ value: "copy" }); copied.close();
    expect(fs.readFileSync(paths.keyPath)).toEqual(fs.readFileSync(sourceKeyPath));
    await target.abort(handoff);
    expect(fs.readFileSync(paths.dbPath, "utf8")).toBe(old.db); expect(fs.readFileSync(paths.keyPath, "utf8")).toBe(old.key); expect(fs.readFileSync(paths.authorizedKeysPath, "utf8")).toBe(old.auth); expect(fs.readFileSync(paths.handoffPath, "utf8")).toBe(old.handoff); expect(fs.readFileSync(paths.plistPath, "utf8")).toBe(old.plist);
    expect(fs.readFileSync(serveLog, "utf8")).toContain("set-config");
    const failedHandoff = "00000000-0000-4000-8000-000000000002";
    await target.prepare({ handoff_id: failedHandoff, source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102", expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: BigInt(bytes.length), action: "PREPARE" });
    target.startSnapshot({ request_id: "bad-request", handoff_id: failedHandoff, total_size: BigInt(bytes.length), sha256: "0".repeat(64), coord_key_pem: fs.readFileSync(sourceKeyPath), authorized_keys: Buffer.from("new-auth"), secret_sha256: "a".repeat(64), expected_worker_fps: ["worker"] });
    await expect(target.appendSnapshot({ handoff_id: failedHandoff, seq: 0, data: bytes, last: true })).rejects.toThrow("coordinator snapshot checksum mismatch");
    expect(fs.readFileSync(paths.dbPath, "utf8")).toBe(old.db);
    expect(fs.readFileSync(paths.keyPath, "utf8")).toBe(old.key);
    expect(fs.readFileSync(paths.authorizedKeysPath, "utf8")).toBe(old.auth);
    expect(fs.readFileSync(paths.handoffPath, "utf8")).toBe(old.handoff);
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
      plistPath: join(root, "fresh-coord.plist"),
    };
    const freshTarget = new CoordTarget(freshPaths, { platform: "darwin", gitSha: "sha", tailnetDnsName: () => "target.ts.net", isLaunchAgentActive: async () => false, restoreLaunchAgent: async () => {} });
    await freshTarget.prepare({ handoff_id: "00000000-0000-4000-8000-000000000003", source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102", expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: BigInt(bytes.length), action: "CHECK" });
    expect(fs.existsSync(freshData)).toBeFalse();
    await freshTarget.prepare({ handoff_id: "00000000-0000-4000-8000-000000000003", source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102", expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: BigInt(bytes.length), action: "PREPARE" });
    expect(fs.existsSync(freshData)).toBeTrue();

    await expect(new CoordTarget(paths, {
      platform: "darwin", gitSha: "stale", tailnetDnsName: () => "target.ts.net",
      isLaunchAgentActive: async () => false, restoreLaunchAgent: async () => {},
    }).prepare({
      handoff_id: handoff, source_url: "https://source.example:4102", target_url: "https://target.ts.net:4102",
      expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: 0n, action: "CHECK",
    })).rejects.toThrow("worker version stale does not match coordinator");

    await expect(new CoordTarget(paths, {
      platform: "darwin", gitSha: "sha", tailnetDnsName: () => "target.ts.net",
      isLaunchAgentActive: async () => false, restoreLaunchAgent: async () => {},
    }).prepare({
      handoff_id: handoff, source_url: "https://source.example:4102", target_url: "https://other.ts.net:4102",
      expected_coord_kid: sourceKey.verifyingKeyKid(), expected_git_sha: "sha", estimated_db_size: 0n, action: "CHECK",
    })).rejects.toThrow("target URL does not match this worker's Tailscale address");

    await expect(new CoordTarget(paths, {
      platform: "darwin", gitSha: "sha", tailnetDnsName: () => "target.ts.net",
      isLaunchAgentActive: async () => false, restoreLaunchAgent: async () => {},
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
