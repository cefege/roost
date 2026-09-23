// Existing quickstart regressions use real temporary service text, SQLite rows,
// and OpenSSH worker keys. Native lifecycle commands are mocked only at the
// process boundary so promotion rollback preserves the same on-disk bytes.
import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { Database } from "bun:sqlite";
import {
  chmodSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { loadWorkerKey } from "../../worker/src/jwt.ts";
import { coordinatorServiceWithEndpoint } from "../src/coordinator-service-definition.ts";
import { isRegisteredWorker } from "../src/quickstart-bootstrap-tokens.ts";
import { resolveQuickstartEndpoint } from "../src/quickstart-endpoint.ts";

interface CommandResult {
  exit: number;
  stdout: string;
  stderr: string;
}

let commandResults: CommandResult[] = [];
let commands: string[][] = [];

mock.module("../src/deploy-exec.ts", () => ({
  run: async (command: string[]): Promise<CommandResult> => {
    commands.push(command);
    return commandResults.shift() ?? { exit: 0, stdout: "", stderr: "" };
  },
}));
mock.module("../src/machine-transaction.ts", () => ({
  acquireMachineTransaction: async () => ({ release: async () => undefined }),
}));
mock.module("../src/quickstart-runtime.ts", () => ({
  waitForCoordHealth: async () => true,
  waitForCoordSpa: async () => true,
  waitForWorkerRegistration: async () => null,
  waitForWorkerRoutability: async () => true,
}));

const {
  _discoverExistingWorker,
  _reactivateCoordinator,
  discoverExistingQuickstartInstall,
  runExistingQuickstart,
} = await import("../src/quickstart-existing-install.ts");

const ENVIRONMENT_KEYS = [
  "ROOST_COORD_UNIT",
  "ROOST_COORD_PLIST",
  "ROOST_WORKER_UNIT",
  "ROOST_WORKER_PLIST",
] as const;

let root = "";
let savedEnvironment: Record<string, string | undefined> = {};

function systemdDefinition(environment: Record<string, string>): string {
  return [
    "[Unit]",
    "Description=Roost test",
    "",
    "[Service]",
    "WorkingDirectory=/tmp/roost-test-workdir",
    ...Object.entries(environment).map(([name, value]) => `Environment=\"${name}=${value}\"`),
    "ExecStart=/usr/bin/true",
    "",
    "[Install]",
    "WantedBy=default.target",
    "",
  ].join("\n");
}

function coordinatorEnvironment(databasePath: string): Record<string, string> {
  return {
    ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
    ROOST_COORDINATOR_DB: databasePath,
    ROOST_COORDINATOR_AUTHORIZED_KEYS: join(root, "authorized_keys.roost"),
    ROOST_TRUST_PROXY: "1",
    ROOST_WEB_PUBLIC_URL: "https://old.example.test",
    ROOST_COORDINATOR_PUBLIC_URL: "https://workers.example.test",
    ROOST_CORS_ALLOWED_ORIGINS: "https://old.example.test,http://127.0.0.1:4103",
  };
}

function createRegistrationDatabase(databasePath: string, fingerprint: string): void {
  const database = new Database(databasePath);
  try {
    database.exec(`
      CREATE TABLE workers (fp TEXT PRIMARY KEY);
      CREATE TABLE authorized_keys (fingerprint TEXT PRIMARY KEY);
    `);
    database.query("INSERT INTO workers (fp) VALUES (?)").run(fingerprint);
    database.query("INSERT INTO authorized_keys (fingerprint) VALUES (?)").run(fingerprint);
  } finally {
    database.close();
  }
}

beforeEach(() => {
  savedEnvironment = Object.fromEntries(ENVIRONMENT_KEYS.map((name) => [name, process.env[name]]));
  for (const name of ENVIRONMENT_KEYS) delete process.env[name];
  root = mkdtempSync(join(tmpdir(), "roost-existing-quickstart-"));
  commandResults = [];
  commands = [];
});

afterEach(() => {
  for (const name of ENVIRONMENT_KEYS) {
    const value = savedEnvironment[name];
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  rmSync(root, { recursive: true, force: true });
});

describe.skipIf(process.platform === "win32")("existing POSIX quickstart", () => {
  test("discovers an enrolled worker from exact installed paths without changing its key bytes", async () => {
    const databasePath = join(root, "coordinator.db");
    const keyPath = join(root, "worker.key");
    const workerPath = join(root, "roost-worker.service");
    const coordinatorPath = join(root, "roost-coord.service");
    const key = await loadWorkerKey(keyPath);
    createRegistrationDatabase(databasePath, key.fingerprint);
    writeFileSync(coordinatorPath, systemdDefinition(coordinatorEnvironment(databasePath)), { mode: 0o600 });
    writeFileSync(workerPath, systemdDefinition({
      ROOST_COORDINATOR_URL: "http://127.0.0.1:4103",
      ROOST_WORKER_KEY_PATH: keyPath,
      ROOST_WORKER_DATA_DIR: root,
      ROOST_WORKER_LABEL: "preserved-worker",
    }), { mode: 0o600 });
    process.env.ROOST_COORD_UNIT = coordinatorPath;
    process.env.ROOST_WORKER_UNIT = workerPath;
    const beforeKey = readFileSync(keyPath);
    const beforeWorkerDefinition = readFileSync(workerPath);

    const installed = discoverExistingQuickstartInstall("linux");
    const worker = await _discoverExistingWorker("linux");

    expect(installed?.environment.ROOST_COORDINATOR_DB).toBe(databasePath);
    expect(worker?.fingerprint).toBe(key.fingerprint);
    expect(worker?.environment.ROOST_COORDINATOR_URL).toBe("http://127.0.0.1:4103");
    expect(isRegisteredWorker(databasePath, key.fingerprint)).toBe(true);
    expect(readFileSync(keyPath)).toEqual(beforeKey);
    expect(readFileSync(workerPath)).toEqual(beforeWorkerDefinition);
  });

  test("registered running worker rerun preserves identity without provisioning or restart", async () => {
    const databasePath = join(root, "coordinator.db");
    const keyPath = join(root, "worker.key");
    const workerPath = join(root, "roost-worker.service");
    const coordinatorPath = join(root, "roost-coord.service");
    const key = await loadWorkerKey(keyPath);
    createRegistrationDatabase(databasePath, key.fingerprint);
    writeFileSync(coordinatorPath, systemdDefinition(coordinatorEnvironment(databasePath)), { mode: 0o600 });
    writeFileSync(workerPath, systemdDefinition({
      ROOST_COORDINATOR_URL: "http://127.0.0.1:4103",
      ROOST_WORKER_KEY_PATH: keyPath,
      ROOST_WORKER_DATA_DIR: root,
      ROOST_WORKER_LABEL: "preserved-worker",
    }), { mode: 0o600 });
    process.env.ROOST_COORD_UNIT = coordinatorPath;
    process.env.ROOST_WORKER_UNIT = workerPath;
    const beforeKey = readFileSync(keyPath);
    const beforeWorkerDefinition = readFileSync(workerPath);
    const installed = discoverExistingQuickstartInstall("linux");
    const endpoint = resolveQuickstartEndpoint([], "linux", installed!.environment);
    let lifecycleReads = 0;
    let provisionAttempts = 0;

    const result = await runExistingQuickstart({
      installed: installed!,
      endpoint,
      invocation: {
        coordinatorUrl: null,
        dryRun: false,
        force: false,
        windowsServiceCredentialStdin: false,
      },
      provisionWorker: async () => { provisionAttempts += 1; },
      readWorkerLifecycle: () => {
        lifecycleReads += 1;
        return "running";
      },
    });

    expect(result?.workerFingerprint).toBe(key.fingerprint);
    expect(result?.databasePath).toBe(databasePath);
    expect(result?.remoteAccessVerified).toBeNull();
    expect(lifecycleReads).toBe(1);
    expect(provisionAttempts).toBe(0);
    expect(commands).toHaveLength(1);
    expect(commands[0]?.join(" ")).toContain("roost-coord.service");
    expect(readFileSync(keyPath)).toEqual(beforeKey);
    expect(readFileSync(workerPath)).toEqual(beforeWorkerDefinition);
  });

  test("refuses a malformed installed key without regenerating or changing it", async () => {
    const workerPath = join(root, "roost-worker.service");
    const keyPath = join(root, "damaged.key");
    writeFileSync(keyPath, "not an OpenSSH key\n", { mode: 0o600 });
    const beforeKey = readFileSync(keyPath);
    writeFileSync(workerPath, systemdDefinition({
      ROOST_COORDINATOR_URL: "http://127.0.0.1:4103",
      ROOST_WORKER_KEY_PATH: keyPath,
      ROOST_WORKER_DATA_DIR: root,
    }), { mode: 0o600 });
    process.env.ROOST_WORKER_UNIT = workerPath;

    await expect(_discoverExistingWorker("linux")).rejects.toThrow(/explicit repair/);
    expect(readFileSync(keyPath)).toEqual(beforeKey);
  });

  test("promotion changes only endpoint entries and retains worker identity directives", () => {
    const databasePath = join(root, "coordinator.db");
    const original = systemdDefinition(coordinatorEnvironment(databasePath));
    const endpoint = resolveQuickstartEndpoint(
      ["--coordinator-url", "https://new.example.test"],
      "linux",
      coordinatorEnvironment(databasePath),
    );
    const rewritten = coordinatorServiceWithEndpoint(original, "linux", endpoint);

    expect(rewritten).toContain("WorkingDirectory=/tmp/roost-test-workdir");
    expect(rewritten).toContain("ROOST_COORDINATOR_DB=" + databasePath);
    expect(rewritten).toContain("ROOST_COORDINATOR_PUBLIC_URL=https://workers.example.test");
    expect(rewritten).toContain("ROOST_WEB_PUBLIC_URL=https://new.example.test");
    expect(rewritten).toContain("ROOST_TRUST_PROXY=1");
    expect(rewritten).toContain("ROOST_CORS_ALLOWED_ORIGINS=https://old.example.test,http://127.0.0.1:4103");
  });

  test("refuses duplicate endpoint entries instead of guessing a promotion target", () => {
    const environment = coordinatorEnvironment(join(root, "coordinator.db"));
    const endpoint = resolveQuickstartEndpoint(
      ["--coordinator-url", "https://new.example.test"],
      "linux",
      environment,
    );
    const duplicate = systemdDefinition(environment).replace(
      "ExecStart=/usr/bin/true",
      "Environment=\"ROOST_WEB_PUBLIC_URL=https://second.example.test\"\nExecStart=/usr/bin/true",
    );

    expect(() => coordinatorServiceWithEndpoint(duplicate, "linux", endpoint))
      .toThrow(/duplicate ROOST_WEB_PUBLIC_URL/);
  });

  test("promotion rewrites launchd endpoint keys without touching separate worker identity", () => {
    const environment = coordinatorEnvironment(join(root, "coordinator.db"));
    const endpoint = resolveQuickstartEndpoint(
      ["--coordinator-url", "https://new.example.test"],
      "darwin",
      environment,
    );
    const plist = `<?xml version="1.0"?><plist><dict>
<key>Label</key><string>com.example.coord</string>
<key>EnvironmentVariables</key><dict>
<key>ROOST_COORDINATOR_BIND</key><string>127.0.0.1:4103</string>
<key>ROOST_COORDINATOR_DB</key><string>${environment.ROOST_COORDINATOR_DB}</string>
<key>ROOST_COORDINATOR_PUBLIC_URL</key><string>https://workers.example.test</string>
<key>ROOST_TRUST_PROXY</key><string>1</string>
<key>ROOST_WEB_PUBLIC_URL</key><string>https://old.example.test</string>
<key>ROOST_CORS_ALLOWED_ORIGINS</key><string>https://old.example.test,http://127.0.0.1:4103</string>
</dict><key>KeepAlive</key><true/></dict></plist>`;

    const rewritten = coordinatorServiceWithEndpoint(plist, "darwin", endpoint);

    expect(rewritten).toContain("<key>ROOST_COORDINATOR_PUBLIC_URL</key><string>https://workers.example.test</string>");
    expect(rewritten).toContain("<key>ROOST_WEB_PUBLIC_URL</key><string>https://new.example.test</string>");
    expect(rewritten).toContain("<key>KeepAlive</key><true/>");
  });

  test("activation failure restores prior service bytes and mode", async () => {
    const databasePath = join(root, "coordinator.db");
    const coordinatorPath = join(root, "roost-coord.service");
    const environment = coordinatorEnvironment(databasePath);
    const original = systemdDefinition(environment);
    writeFileSync(coordinatorPath, original, { mode: 0o640 });
    process.env.ROOST_COORD_UNIT = coordinatorPath;
    const installed = discoverExistingQuickstartInstall("linux");
    const endpoint = resolveQuickstartEndpoint(
      ["--coordinator-url", "https://new.example.test"],
      "linux",
      environment,
    );
    commandResults = [
      { exit: 1, stdout: "", stderr: "activation failed" },
      { exit: 0, stdout: "", stderr: "" },
    ];

    await expect(_reactivateCoordinator(installed!, endpoint, true)).rejects.toThrow(/promotion failed/);
    expect(readFileSync(coordinatorPath, "utf8")).toBe(original);
    expect(statSync(coordinatorPath).mode & 0o777).toBe(0o640);
    expect(commands).toHaveLength(2);
  });

  test("an interrupted persisted promotion is reactivated unchanged on the next no-URL rerun", async () => {
    const databasePath = join(root, "coordinator.db");
    const coordinatorPath = join(root, "roost-coord.service");
    const environment = coordinatorEnvironment(databasePath);
    const original = systemdDefinition(environment);
    const promotion = resolveQuickstartEndpoint(
      ["--coordinator-url", "https://new.example.test"],
      "linux",
      environment,
    );
    const persisted = coordinatorServiceWithEndpoint(original, "linux", promotion);
    writeFileSync(coordinatorPath, persisted, { mode: 0o600 });
    process.env.ROOST_COORD_UNIT = coordinatorPath;
    const installed = discoverExistingQuickstartInstall("linux");
    const rerunEndpoint = resolveQuickstartEndpoint([], "linux", installed!.environment);

    await _reactivateCoordinator(installed!, rerunEndpoint, false);

    expect(readFileSync(coordinatorPath, "utf8")).toBe(persisted);
    expect(commands).toHaveLength(1);
    expect(rerunEndpoint.webPublicUrl).toBe("https://new.example.test");
  });
});
