import { afterEach, describe, expect, test } from "bun:test";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import {
  buildWindowsServiceDefinitions,
  retargetWindowsUpdaterDefinition,
  WINDOWS_SERVICE_NAMES,
  type RoostServiceRole,
  type WindowsServiceDefinition,
  type WindowsServiceManager,
  type WindowsServiceSnapshot,
  type WindowsServiceSnapshotSet,
  type WindowsServiceState,
} from "../src/service-ctl.ts";
import {
  runWindowsUpdateBroker,
  type ServiceHealthProver,
  type WindowsUpdateBrokerDeps,
  type WindowsUpdateNative,
} from "../src/windows/windows-update-broker.ts";
import { fixtureHealthTable, readHealthByRole, stubFetch } from "./test-helpers.ts";
import {
  handleUpdateBrokerCommand,
  compareWindowsReleaseIdentity,
  type WindowsUpdateBrokerCommand,
  type WindowsUpdateControlDeps,
  readPublishedWindowsUpdateProgress,
  windowsUpdateRequestDirectory,
} from "../src/windows/windows-update-control.ts";
import {
  DurableWindowsUpdateJournalStore,
  appendWindowsUpdateProgress,
  assertLegacyWindowsUpdateJournal,
  assertWindowsUpdateJournal,
  createWindowsUpdateJournal,
  parseWindowsUpdateJournal,
  sha256Hex,
  type WindowsReleaseFile,
  type WindowsReleaseManifestV1,
  type WindowsUpdateJournal,
  type WindowsUpdateJournalStore,
  type WindowsUpdateJournalV1,
  type WindowsUpdateJournalV2,
} from "../src/windows/windows-update-journal.ts";

const ROLES = ["keeper", "worker", "coordinator", "updater"] as const satisfies readonly RoostServiceRole[];
const PUBLISHER_SHA256 = "a".repeat(64);
const roots: string[] = [];

type ServiceDefinitions = Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>;

class TempJournalStore implements WindowsUpdateJournalStore {
  constructor(readonly path: string) {}

  async load(): Promise<WindowsUpdateJournal | null> {
    if (!existsSync(this.path)) return null;
    return parseWindowsUpdateJournal(readFileSync(this.path, "utf8"));
  }

  async save(journal: WindowsUpdateJournalV2): Promise<void> {
    assertWindowsUpdateJournal(journal);
    atomicWrite(this.path, `${JSON.stringify(journal)}\n`);
  }
  writeRaw(journal: WindowsUpdateJournalV1): void {
    atomicWrite(this.path, `${JSON.stringify(journal)}\n`);
  }


  persistedText(): string {
    return readFileSync(this.path, "utf8");
  }
}

class StatefulServiceManager implements WindowsServiceManager {
  private readonly priorDefinitions: ServiceDefinitions;
  private definitions: Record<RoostServiceRole, WindowsServiceDefinition>;
  private states: Record<RoostServiceRole, WindowsServiceState>;

  constructor(
    definitions: ServiceDefinitions,
    states: Readonly<Record<RoostServiceRole, WindowsServiceState>>,
  ) {
    this.priorDefinitions = clone(definitions);
    this.definitions = clone(definitions) as Record<RoostServiceRole, WindowsServiceDefinition>;
    this.states = clone(states);
  }

  async query(role: RoostServiceRole): Promise<WindowsServiceSnapshot> {
    return clone(this.snapshotFor(role));
  }

  async snapshot(): Promise<WindowsServiceSnapshotSet> {
    return clone(this.snapshotSet());
  }

  async install(definition: WindowsServiceDefinition): Promise<WindowsServiceSnapshot> {
    return await this.configure(definition);
  }

  async configure(definition: WindowsServiceDefinition): Promise<WindowsServiceSnapshot> {
    this.definitions[definition.role] = clone(definition);
    return await this.query(definition.role);
  }

  async start(role: RoostServiceRole): Promise<WindowsServiceSnapshot> {
    if (this.states[role] === "running") throw new Error(`duplicate start of ${role}`);
    this.states[role] = "running";
    return await this.query(role);
  }

  async stop(role: RoostServiceRole): Promise<WindowsServiceSnapshot> {
    this.states[role] = "stopped";
    return await this.query(role);
  }

  async restore(
    snapshot: WindowsServiceSnapshotSet,
    options?: { restoreLifecycleRoles?: readonly RoostServiceRole[] },
  ): Promise<WindowsServiceSnapshotSet> {
    this.definitions = clone(this.priorDefinitions) as Record<RoostServiceRole, WindowsServiceDefinition>;
    const lifecycleRoles = options?.restoreLifecycleRoles ?? (["worker", "coordinator"] as const);
    for (const role of lifecycleRoles) this.states[role] = snapshot[role].state;
    return await this.snapshot();
  }

  async provisionServiceLogon(_account: string): Promise<void> {}

  /** Elevated-install only, so no update-broker path may reach it. */
  async provisionServiceSecurity(_interactiveSid: string): Promise<void> {
    throw new Error("the update broker must not provision service security");
  }

  stateVector(): Readonly<Record<RoostServiceRole, WindowsServiceState>> {
    return clone(this.states);
  }

  definitionVector(): ServiceDefinitions {
    return clone(this.definitions);
  }

  snapshotSet(): WindowsServiceSnapshotSet {
    return Object.fromEntries(ROLES.map((role) => [role, this.snapshotFor(role)])) as unknown as WindowsServiceSnapshotSet;
  }

  private snapshotFor(role: RoostServiceRole): WindowsServiceSnapshot {
    const definition = this.definitions[role];
    return {
      role,
      name: WINDOWS_SERVICE_NAMES[role],
      installed: true,
      state: this.states[role],
      startMode: definition.startMode,
      imagePath: definition.imagePath,
      account: definition.account,
      dependencies: definition.dependencies.map((dependency) => WINDOWS_SERVICE_NAMES[dependency]),
      displayName: definition.displayName,
      description: definition.description,
      recoveryPolicy: {
        resetPeriodSeconds: 86_400,
        rebootMessage: "",
        command: "",
        actions: [
          { type: "restart", delayMs: 5_000 },
          { type: "restart", delayMs: 30_000 },
          { type: "restart", delayMs: 60_000 },
        ],
        actionsOnNonCrashFailures: true,
      },
      environment: definition.environment,
      serviceSidType: "unrestricted",
      securityDescriptor: "D:P(A;;FA;;;SY)",
    };
  }
}

class FixtureHealthProver implements ServiceHealthProver {
  constructor(
    private readonly manager: StatefulServiceManager,
    private readonly currentManifestPath: string,
    private readonly priorDefinitions: ServiceDefinitions,
    private readonly nextDefinitions: ServiceDefinitions,
    private readonly failForward: boolean,
  ) {}
  read = readHealthByRole(fixtureHealthTable());

  async prove(
    role: "worker" | "coordinator",
    journal: Readonly<WindowsUpdateJournalV2>,
    mode: "forward" | "rollback",
  ): Promise<void> {
    if (this.manager.stateVector()[role] !== "running") throw new Error(`${role} is not running`);
    const expectedPhase = mode === "forward" ? "services-restored" : "rollback-current-manifest-restored";
    if (journal.phase !== expectedPhase) throw new Error(`${mode} health proof ran at ${journal.phase}`);

    const expectedManifest = mode === "forward"
      ? `${JSON.stringify(journal.currentManifest.next)}\n`
      : journal.currentManifest.priorRaw;
    const actualManifest = existsSync(this.currentManifestPath)
      ? readFileSync(this.currentManifestPath, "utf8")
      : null;
    if (actualManifest !== expectedManifest) throw new Error(`${mode} health proof observed the wrong current manifest`);

    const actualDefinitions = this.manager.definitionVector();
    const expectedDefinitions = mode === "forward" ? this.nextDefinitions : this.priorDefinitions;
    if (!ROLES.every((serviceRole) =>
      JSON.stringify(actualDefinitions[serviceRole]) === JSON.stringify(expectedDefinitions[serviceRole])
    )) {
      throw new Error(`${mode} health proof observed the wrong service definitions`);
    }
    if (mode === "forward" && this.failForward) throw new Error("injected post-stop health fault");
  }
}

interface BrokerFixture {
  root: string;
  serviceDir: string;
  versionsDir: string;
  currentManifestPath: string;
  serviceDefinitionsPath: string;
  priorCurrentRaw: string;
  priorDefinitions: ServiceDefinitions;
  nextDefinitions: ServiceDefinitions;
  journal: WindowsUpdateJournalV2;
  store: TempJournalStore;
  manager: StatefulServiceManager;
  native: WindowsUpdateNative;
  acquireTransaction: NonNullable<WindowsUpdateBrokerDeps["acquireTransaction"]>;
  deps: WindowsUpdateBrokerDeps;
  assetBytes: Readonly<Record<string, string>>;
}

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

describe("Windows update broker durability", () => {
  test("persists every forward phase in order and commits only after service health is proven", async () => {
    const fixture = await createBrokerFixture({ worker: true, coordinator: true });

    const result = await runWindowsUpdateBroker(fixture.deps);
    if (result === null) throw new Error("broker unexpectedly reported an idle journal");
    const durable = await fixture.store.load();

    expect(durable).toEqual(result);
    expect(result.state).toBe("succeeded");
    expect(result.phase).toBe("cleanup-complete");
    expect(result.progress.map(({ phase }) => phase)).toEqual([
      "prepared",
      "broker-started",
      "assets-staged",
      "stable-artifacts-snapshotted",
      "services-stopped",
      "stable-artifacts-promoted",
      "updater-config-switched",
      "current-manifest-switched",
      "services-restored",
      "health-proven",
      "committed",
      "cleanup-complete",
    ]);
    expect(result.progress.map(({ sequence }) => sequence)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    expect(result.progress.at(-1)).toMatchObject({
      phase: "cleanup-complete",
      terminal: true,
      success: true,
    });
    expect(result.progress.slice(0, -1).every(({ terminal }) => !terminal)).toBe(true);
    expect(result.stoppedRoles).toEqual(["worker", "coordinator", "keeper"]);
    expect(result.restoredRoles).toEqual(["keeper", "coordinator", "worker"]);
    expect(fixture.manager.stateVector()).toEqual({
      keeper: "running",
      worker: "running",
      coordinator: "running",
      updater: "stopped",
    });
    expect(fixture.manager.definitionVector()).toEqual(fixture.nextDefinitions);
    expect(JSON.parse(readFileSync(fixture.serviceDefinitionsPath, "utf8")).services).toEqual(fixture.nextDefinitions);
    expect(readFileSync(fixture.currentManifestPath, "utf8")).toBe(
      `${JSON.stringify(fixture.journal.currentManifest.next)}\n`,
    );
    for (const [path, bytes] of Object.entries(fixture.assetBytes)) {
      expect(readFileSync(join(fixture.journal.paths.newVersionDir, path), "utf8")).toBe(bytes);
    }
  });

  test("a post-stop fault restores the exact prior vector, configs, and manifest before terminal rollback", async () => {
    const fixture = await createBrokerFixture({ worker: true, coordinator: false }, true);

    const result = await runWindowsUpdateBroker(fixture.deps);
    if (result === null) throw new Error("broker unexpectedly reported an idle journal");
    const durable = await fixture.store.load();

    expect(durable).toEqual(result);
    expect(result.state).toBe("rolled-back");
    expect(result.phase).toBe("rolled-back");
    expect(result.failure).toMatchObject({
      forwardPhase: "services-restored",
      error: expect.stringContaining("injected post-stop health fault"),
    });
    expect(result.progress.map(({ phase }) => phase)).toEqual([
      "prepared",
      "broker-started",
      "assets-staged",
      "stable-artifacts-snapshotted",
      "services-stopped",
      "stable-artifacts-promoted",
      "updater-config-switched",
      "current-manifest-switched",
      "services-restored",
      "rollback-started",
      "rollback-services-stopped",
      "rollback-stable-artifacts-restored",
      "rollback-configs-restored",
      "rollback-current-manifest-restored",
      "rollback-services-restored",
      "rolled-back",
    ]);
    expect(result.progress.map(({ sequence }) => sequence)).toEqual([
      1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    ]);
    expect(result.progress.at(-1)).toMatchObject({
      phase: "rolled-back",
      terminal: true,
      success: false,
      error: expect.stringContaining("injected post-stop health fault"),
    });
    expect(result.progress.slice(0, -1).every(({ terminal }) => !terminal)).toBe(true);
    expect(fixture.manager.stateVector()).toEqual({
      keeper: "running",
      worker: "running",
      coordinator: "stopped",
      updater: "stopped",
    });
    expect(fixture.manager.definitionVector()).toEqual(fixture.priorDefinitions);
    expect(JSON.parse(readFileSync(fixture.serviceDefinitionsPath, "utf8")).services).toEqual(fixture.priorDefinitions);
    expect(readFileSync(fixture.currentManifestPath, "utf8")).toBe(fixture.priorCurrentRaw);
    expect(existsSync(fixture.journal.paths.newVersionDir)).toBe(false);
    expect(existsSync(join(
      fixture.versionsDir,
      `.rolled-back-2.0.0-${sha256Hex(fixture.journal.transactionId).slice(0, 16)}`,
    ))).toBe(true);
  });
});

describe("Windows update schema migration", () => {
  test("accepts buildless schema 1 state while schema 2 requires every immutable build field", async () => {
    const fixture = await createBrokerFixture({ worker: true, coordinator: true });
    const legacy = legacyJournalFromV2(fixture.journal);

    expect(() => assertLegacyWindowsUpdateJournal(legacy)).not.toThrow();
    expect(parseWindowsUpdateJournal(JSON.stringify(legacy)).schemaVersion).toBe(1);
    expect(legacy.targetBuild).toBeUndefined();
    expect(legacy.currentManifest.next.build).toBeUndefined();
    expect(legacy.healthBefore.worker?.build).toBeUndefined();

    const missingTargetBuild: { targetBuild?: string } = clone(fixture.journal);
    delete missingTargetBuild.targetBuild;
    expect(() => assertWindowsUpdateJournal(missingTargetBuild))
      .toThrow("journal.targetBuild must be a lowercase immutable build id");

    const malformedCurrentBuild = clone(fixture.journal);
    malformedCurrentBuild.currentManifest.next.build = "mutable";
    expect(() => assertWindowsUpdateJournal(malformedCurrentBuild))
      .toThrow("current build must be a lowercase immutable build id");

    const malformedHealthBuild = clone(fixture.journal);
    if (!malformedHealthBuild.healthBefore.worker) throw new Error("fixture worker checkpoint is missing");
    malformedHealthBuild.healthBefore.worker.build = "build-1";
    expect(() => assertWindowsUpdateJournal(malformedHealthBuild))
      .toThrow("journal healthBefore worker.build must be a lowercase immutable build id");
  });

  test("an active pre-stop schema 1 journal fails closed without deleting diagnostic state", async () => {
    const fixture = await createBrokerFixture({ worker: true, coordinator: true });
    const legacy = legacyJournalFromV2(fixture.journal, "assets-staged");
    const legacyPath = join(fixture.serviceDir, "update-v1.json");
    rmSync(fixture.store.path, { force: true });
    writeFileSync(fixture.currentManifestPath, legacy.currentManifest.priorRaw ?? "");
    writeFileSync(legacyPath, `${JSON.stringify(legacy)}\n`);
    const before = readFileSync(legacyPath, "utf8");
    const servicesBefore = fixture.manager.stateVector();
    const store = new DurableWindowsUpdateJournalStore(fixture.store.path, legacyPath);

    await expect(runWindowsUpdateBroker({
      ...fixture.deps,
      store,
      serviceDir: fixture.serviceDir,
      versionsDir: fixture.versionsDir,
      currentManifestPath: fixture.currentManifestPath,
    })).rejects.toThrow(
      "legacy Windows update topology requires signed elevated installer migration before mutation",
    );

    expect(existsSync(fixture.store.path)).toBe(false);
    expect(readFileSync(legacyPath, "utf8")).toBe(before);
    expect(fixture.manager.stateVector()).toEqual(servicesBefore);
  });

  test("an incompatible post-stop schema 1 journal fails closed without deleting diagnostic state", async () => {
    const fixture = await createBrokerFixture({ worker: true, coordinator: false });
    const legacy = legacyJournalFromV2(fixture.journal, "services-stopped");
    const legacyPath = join(fixture.serviceDir, "update-v1.json");
    rmSync(fixture.store.path, { force: true });
    writeFileSync(legacyPath, `${JSON.stringify(legacy)}\n`);
    const before = readFileSync(legacyPath, "utf8");
    const servicesBefore = fixture.manager.stateVector();
    const store = new DurableWindowsUpdateJournalStore(fixture.store.path, legacyPath);

    await expect(runWindowsUpdateBroker({
      ...fixture.deps,
      store,
      serviceDir: fixture.serviceDir,
      versionsDir: fixture.versionsDir,
      currentManifestPath: fixture.currentManifestPath,
    })).rejects.toThrow(
      "legacy Windows update topology requires signed elevated installer migration before mutation",
    );

    expect(existsSync(fixture.store.path)).toBe(false);
    expect(readFileSync(legacyPath, "utf8")).toBe(before);
    const { releasePackage } = fixture.journal;
    if (releasePackage === null) throw new Error("fixture release package is missing");
    expect(existsSync(releasePackage.path)).toBe(true);
    expect(fixture.manager.stateVector()).toEqual(servicesBefore);
  });
});


describe("Windows update control replay", () => {
  test("START only enqueues an idempotent request without reading the protected journal", async () => {
    const fixture = await createBrokerFixture({ worker: true, coordinator: false });
    let journal = appendWindowsUpdateProgress(
      fixture.journal,
      "broker-started",
      "updater resumed",
      { now: new Date("2026-08-16T00:00:10.000Z") },
    );
    journal = appendWindowsUpdateProgress(
      journal,
      "assets-staged",
      "assets verified",
      { now: new Date("2026-08-16T00:00:11.000Z") },
    );
    await fixture.store.save(journal);
    const command = startCommand(journal.jobId);
    const before = fixture.store.persistedText();

    const first = await handleUpdateBrokerCommand(command, controlDeps(fixture));
    const second = await handleUpdateBrokerCommand(command, controlDeps(fixture));

    expect(first).toEqual(second);
    expect(first).toEqual([{
      requestId: command.requestId,
      jobId: command.jobId,
      sequence: 0,
      phase: "admission-requested",
      message: "signed update admission queued for the constrained updater service",
      terminal: false,
      success: false,
      error: "",
    }]);
    expect(fixture.manager.stateVector().updater).toBe("running");
    expect(fixture.store.persistedText()).toBe(before);
    expect(await fixture.store.load()).toEqual(journal);
  });

  test("release identity is idempotent only for the exact immutable build", () => {
    const current = { version: "v2.0.0+metadata", build: "a".repeat(40) };

    expect(compareWindowsReleaseIdentity(
      current,
      { version: "2.0.0", build: "a".repeat(40) },
    )).toBe("same");
    expect(compareWindowsReleaseIdentity(
      current,
      { version: "2.0.0", build: "b".repeat(40) },
    )).toBe("collision");
    expect(compareWindowsReleaseIdentity(
      current,
      { version: "2.0.1", build: "b".repeat(40) },
    )).toBe("different");
  });
  test("isolates interactive update admissions from service relocation requests", () => {
    const serviceDir = "C:\\ProgramData\\Roost\\service";
    expect(windowsUpdateRequestDirectory(serviceDir, "interactive")).toBe(
      join(serviceDir, "requests", "interactive-update"),
    );
    expect(windowsUpdateRequestDirectory(serviceDir, "worker")).toBe(
      join(serviceDir, "requests"),
    );
    expect(windowsUpdateRequestDirectory(serviceDir, "coordinator")).toBe(
      join(serviceDir, "requests"),
    );
  });


  test("STATUS cursor zero returns sequence one and every later durable progress frame", async () => {
    const fixture = await createBrokerFixture({ worker: true, coordinator: false });
    let journal = appendWindowsUpdateProgress(
      fixture.journal,
      "broker-started",
      "updater resumed",
      { now: new Date("2026-08-16T00:00:10.000Z") },
    );
    journal = appendWindowsUpdateProgress(
      journal,
      "assets-staged",
      "assets verified",
      { now: new Date("2026-08-16T00:00:11.000Z") },
    );
    await fixture.store.save(journal);

    const command = statusCommand(journal.jobId, 0);
    const frames = await handleUpdateBrokerCommand(command, controlDeps(fixture));
    const laterFrames = await handleUpdateBrokerCommand(
      statusCommand(journal.jobId, 1),
      controlDeps(fixture),
    );

    expect(frames.map(({ sequence }) => sequence)).toEqual([1, 2, 3]);
    expect(frames.map(({ phase }) => phase)).toEqual(["prepared", "broker-started", "assets-staged"]);
    expect(frames.map(({ message }) => message)).toEqual(journal.progress.map(({ message }) => message));
    expect(frames.every(({ requestId }) => requestId === command.requestId)).toBe(true);
    expect(laterFrames.map(({ sequence }) => sequence)).toEqual([2, 3]);
    expect(await handleUpdateBrokerCommand(statusCommand("another-job", 0), controlDeps(fixture))).toEqual([]);
  });

  test("published STATUS reads only the bounded status projection", async () => {
    const fixture = await createBrokerFixture({ worker: true, coordinator: false });
    const raw = Buffer.from(`${JSON.stringify({
      schemaVersion: 1,
      jobId: fixture.journal.jobId,
      progress: fixture.journal.progress,
    })}\n`);
    let observedProfile = "";
    const frames = await readPublishedWindowsUpdateProgress(
      fixture.journal.jobId,
      0,
      "published-status",
      fixture.serviceDir,
      async (_path, profile, maxBytes) => {
        observedProfile = profile;
        expect(maxBytes).toBe(1024 * 1024);
        return raw;
      },
    );

    expect(observedProfile).toBe("status");
    expect(frames.map(({ sequence }) => sequence)).toEqual([1]);
    expect(frames.map(({ phase }) => phase)).toEqual(["prepared"]);
    expect(frames.every(({ requestId }) => requestId === "published-status")).toBe(true);
  });

  test("malformed requests fail locally while active-job arbitration remains updater-owned", async () => {
    const fixture = await createBrokerFixture({ worker: true, coordinator: false });
    const durableBefore = fixture.store.persistedText();
    const malformed = {
      ...startCommand(fixture.journal.jobId),
      action: "STOP",
    } as unknown as WindowsUpdateBrokerCommand;

    await expect(handleUpdateBrokerCommand(malformed, controlDeps(fixture))).rejects.toThrow(
      "unknown update action",
    );
    const queued = await handleUpdateBrokerCommand(
      startCommand("different-job"),
      controlDeps(fixture),
    );
    await expect(
      handleUpdateBrokerCommand(statusCommand(fixture.journal.jobId, -1), controlDeps(fixture)),
    ).rejects.toThrow("afterSequence must be non-negative");

    expect(queued.map(({ phase }) => phase)).toEqual(["admission-requested"]);
    expect(fixture.store.persistedText()).toBe(durableBefore);
    expect(await fixture.store.load()).toEqual(fixture.journal);
    expect(fixture.manager.stateVector().updater).toBe("running");
  });
});

async function createBrokerFixture(
  active: Readonly<{ worker: boolean; coordinator: boolean }>,
  failForwardHealth = false,
): Promise<BrokerFixture> {
  const root = mkdtempSync(join(tmpdir(), "roost-windows-update-broker-"));
  roots.push(root);
  const serviceDir = join(root, "service");
  const versionsDir = join(root, "versions");
  const priorVersionDir = join(versionsDir, "1.0.0");
  const newVersionDir = join(versionsDir, "2.0.0");
  const stagingDir = join(serviceDir, "updates", sha256Hex("job-1"));
  const currentManifestPath = join(serviceDir, "current.json");
  const serviceDefinitionsPath = join(serviceDir, "service-definitions.json");
  const manifestPath = join(stagingDir, "roost-windows-x64.manifest.json");
  const signaturePath = `${manifestPath}.p7s`;
  const packagePath = join(stagingDir, "roost-windows-x64.zip");
  mkdirSync(priorVersionDir, { recursive: true });
  mkdirSync(stagingDir, { recursive: true });

  const priorDefinitions = definitionsFor(priorVersionDir, serviceDir);
  const nextDefinitions = {
    ...priorDefinitions,
    updater: retargetWindowsUpdaterDefinition(
      priorDefinitions.updater,
      newVersionDir,
    ),
  };
  const stateVector: Record<RoostServiceRole, WindowsServiceState> = {
    keeper: "running",
    worker: active.worker ? "running" : "stopped",
    coordinator: active.coordinator ? "running" : "stopped",
    updater: "stopped",
  };
  const stableBin = join(root, "bin");
  mkdirSync(stableBin, { recursive: true });
  writeFileSync(join(stableBin, "shawl.exe"), "signed-shawl-1.0.0\n");
  writeFileSync(join(stableBin, "roost.exe"), "signed-launcher-1.0.0\n");
  const manager = new StatefulServiceManager(priorDefinitions, stateVector);
  atomicWrite(
    serviceDefinitionsPath,
    `${JSON.stringify({ schemaVersion: 2, services: priorDefinitions }, null, 2)}\n`,
  );

  const assetBytes = {
    "roost.exe": "signed-roost-2.0.0\n",
    "roost-win-helper.exe": "signed-helper-2.0.0\n",
    "shawl.exe": "signed-shawl-1.9.0\n",
  } as const;
  const assets: WindowsReleaseFile[] = Object.entries(assetBytes).map(([path, bytes]) => ({
    path,
    sha256: sha256Hex(bytes),
    size: Buffer.byteLength(bytes),
    authenticodeRequired: true,
  }));
  const packageBytes = "fixture-zip-containing-three-signed-assets\n";
  const releaseManifest: WindowsReleaseManifestV1 = {
    schemaVersion: 1,
    version: "2.0.0",
    build: "2".repeat(40),
    platform: "win32",
    arch: "x64",
    publishedAt: "2026-08-16T00:00:00.000Z",
    package: {
      name: "roost-windows-x64.zip",
      sha256: sha256Hex(packageBytes),
      size: Buffer.byteLength(packageBytes),
    },
    files: assets,
    shawl: { version: "1.9.0", upstreamSha256: "b".repeat(64) },
  };
  const manifestRaw = `${JSON.stringify(releaseManifest)}\n`;
  const signature = `fixture-cms:${sha256Hex(manifestRaw)}:${PUBLISHER_SHA256}`;
  writeFileSync(manifestPath, manifestRaw);
  writeFileSync(signaturePath, signature);
  writeFileSync(packagePath, packageBytes);

  const priorCurrent = {
    schemaVersion: 2 as const,
    version: "1.0.0",
    build: "1".repeat(40),
    versionDir: priorVersionDir,
    files: [],
    manifestUrl: "https://updates.example.test/1.0.0/roost-windows-x64.manifest.json",
    manifestSha256: "c".repeat(64),
    publisherSha256: PUBLISHER_SHA256,
  };
  const priorCurrentRaw = `${JSON.stringify(priorCurrent, null, 2)}\n`;
  writeFileSync(currentManifestPath, priorCurrentRaw);

  let tick = 0;
  const now = () => new Date(Date.UTC(2026, 7, 16, 0, 0, tick++));
  const journal = createWindowsUpdateJournal({
    transactionId: "transaction-1",
    jobId: "job-1",
    targetVersion: releaseManifest.version,
    targetBuild: releaseManifest.build,
    signedManifest: {
      url: "https://updates.example.test/2.0.0/roost-windows-x64.manifest.json",
      signatureUrl: "https://updates.example.test/2.0.0/roost-windows-x64.manifest.json.p7s",
      path: manifestPath,
      signaturePath,
      sha256: sha256Hex(manifestRaw),
      publisherSha256: PUBLISHER_SHA256,
    },
    releasePackage: {
      url: "https://updates.example.test/2.0.0/roost-windows-x64.zip",
      path: packagePath,
      sha256: releaseManifest.package.sha256,
      size: releaseManifest.package.size,
    },
    assets,
    paths: {
      priorVersionDir,
      newVersionDir,
      stagingDir,
      currentManifestPath,
    },
    currentManifest: {
      priorRaw: priorCurrentRaw,
      next: {
        schemaVersion: 2,
        version: releaseManifest.version,
        build: releaseManifest.build,
        versionDir: newVersionDir,
        files: assets.map(({ path, sha256, size }) => ({ path, sha256, size })),
        manifestUrl: "https://updates.example.test/2.0.0/roost-windows-x64.manifest.json",
        manifestSha256: sha256Hex(manifestRaw),
        publisherSha256: PUBLISHER_SHA256,
      },
    },
    currentManifestSnapshot: {
      sha256: sha256Hex(priorCurrentRaw),
      size: Buffer.byteLength(priorCurrentRaw),
      securityDescriptor: "non-windows-test-security-descriptor",
    },
    serviceSnapshot: manager.snapshotSet(),
    priorServiceDefinitions: priorDefinitions,
    nextServiceDefinitions: Object.values(nextDefinitions),
    runningBefore: {
      keeper: true,
      worker: active.worker,
      coordinator: active.coordinator,
      updater: false,
    },
    healthBefore: {
      ...(active.worker
        ? {
            worker: {
              version: "1.0.0",
              build: "1".repeat(40),
              processEpoch: "worker-epoch-1",
              coordinatorUrl: "https://coordinator.example.test",
            },
          }
        : {}),
      ...(active.coordinator
        ? {
            coordinator: {
              version: "1.0.0",
              build: "1".repeat(40),
              processEpoch: "coordinator-epoch-1",
            },
          }
        : {}),
    },
    now,
  });
  const store = new TempJournalStore(join(serviceDir, "update-v2.json"));
  await store.save(journal);

  const native = nativeFixture(assetBytes, packageBytes, manifestRaw, signature);
  const health = new FixtureHealthProver(
    manager,
    currentManifestPath,
    priorDefinitions,
    nextDefinitions,
    failForwardHealth,
  );
  const acquireTransaction = transactionFixture(root);
  const deps: WindowsUpdateBrokerDeps = {
    store,
    services: manager,
    native,
    health,
    serviceDir,
    versionsDir,
    currentManifestPath,
    acquireTransaction,
    writeCurrentManifest: async (path, contents) => {
      if (contents === null) rmSync(path, { force: true });
      else atomicWrite(path, contents);
    },
    now,
  };
  return {
    root,
    serviceDir,
    versionsDir,
    currentManifestPath,
    serviceDefinitionsPath,
    priorCurrentRaw,
    priorDefinitions,
    nextDefinitions,
    journal,
    store,
    manager,
    native,
    acquireTransaction,
    deps,
    assetBytes,
  };
}

function nativeFixture(
  assetBytes: Readonly<Record<string, string>>,
  packageBytes: string,
  manifestRaw: string,
  signature: string,
): WindowsUpdateNative {
  return {
    assertUpdaterServiceContext: async () => {},
    probeExclusiveOpen: async (path) => path.includes("1.0.0"),
    verifyCmsDetached: async (manifestPath, signaturePath, publisherSha256) => {
      if (publisherSha256 !== PUBLISHER_SHA256) throw new Error("unexpected publisher pin");
      if (readFileSync(manifestPath, "utf8") !== manifestRaw) throw new Error("CMS manifest bytes changed");
      if (readFileSync(signaturePath, "utf8") !== signature) throw new Error("CMS signature bytes changed");
    },
    verifyAuthenticode: async (path, publisherSha256) => {
      if (publisherSha256 !== PUBLISHER_SHA256) throw new Error("unexpected Authenticode publisher pin");
      const actual = readFileSync(path);
      const signed = Object.values(assetBytes).some((bytes) => actual.equals(Buffer.from(bytes)));
      if (!signed) throw new Error(`unsigned fixture asset: ${path}`);
    },
    extractZip: async (packagePath, destination, files) => {
      if (readFileSync(packagePath, "utf8") !== packageBytes) throw new Error("unexpected package bytes");
      for (const file of files) {
        const bytes = assetBytes[file.path];
        if (bytes === undefined || sha256Hex(bytes) !== file.sha256 || Buffer.byteLength(bytes) !== file.size) {
          throw new Error(`archive metadata mismatch: ${file.path}`);
        }
        const path = join(destination, file.path);
        mkdirSync(dirname(path), { recursive: true });
        writeFileSync(path, bytes);
      }
    },
    protectArtifacts: async (path) => {
      for (const assetPath of Object.keys(assetBytes)) {
        if (!existsSync(join(path, assetPath))) throw new Error(`cannot protect missing asset: ${assetPath}`);
      }
    },
  };
}

function definitionsFor(versionDir: string, serviceDir: string): ServiceDefinitions {
  return buildWindowsServiceDefinitions({
    executablePath: join(versionDir, "roost.exe"),
    shawlPath: join(dirname(dirname(versionDir)), "bin", "shawl.exe"),
    serviceLauncherPath: join(dirname(dirname(versionDir)), "bin", "roost.exe"),
    windowsHelperPath: join(versionDir, "roost-win-helper.exe"),
    account: ".\\roost-test",
    coordinatorHost: true,
    serviceDir,
    commonEnvironment: {
      ROOST_SERVICE_DIR: serviceDir,
      ROOST_VERSIONS_DIR: dirname(versionDir),
      ROOST_WINDOWS_PUBLISHER_SHA256: PUBLISHER_SHA256,
    },
  });
}


function transactionFixture(
  root: string,
): NonNullable<WindowsUpdateBrokerDeps["acquireTransaction"]> {
  let held = false;
  return async (kind, journalPath) => {
    if (held) throw new Error("fixture machine transaction already held");
    held = true;
    return {
      schemaVersion: 1,
      kind,
      journalPath,
      ownerPid: process.pid,
      processEpoch: "fixture-process-epoch",
      acquiredAt: "2026-08-16T00:00:00.000Z",
      lockPath: join(root, "machine-transaction.lock"),
      release: async () => {
        held = false;
      },
    };
  };
}

function controlDeps(fixture: BrokerFixture): WindowsUpdateControlDeps {
  const unusedFetch = stubFetch(() => {
    throw new Error("same-job replay must not download");
  });
  const requestDir = join(fixture.serviceDir, "requests");
  return {
    store: fixture.store,
    services: fixture.manager,
    native: fixture.native,
    fetch: unusedFetch,
    serviceDir: fixture.serviceDir,
    versionsDir: fixture.versionsDir,
    currentManifestPath: fixture.currentManifestPath,
    requestDir,
    createRequest: async (path, contents) => {
      mkdirSync(dirname(path), { recursive: true });
      if (existsSync(path)) {
        if (!readFileSync(path).equals(Buffer.from(contents))) {
          throw new Error("existing updater request bytes changed");
        }
        return;
      }
      writeFileSync(path, contents);
    },
    probeHealth: async () => {
      throw new Error("same-job replay must not probe health");
    },
    platform: "win32",
  };
}

function startCommand(jobId: string): WindowsUpdateBrokerCommand {
  return {
    requestId: "start-request",
    jobId,
    action: "START",
    manifestUrl: "https://updates.example.test/2.0.0/roost-windows-x64.manifest.json",
    signatureUrl: "https://updates.example.test/2.0.0/roost-windows-x64.manifest.json.p7s",
    manifestSha256: "d".repeat(64),
    publisherSha256: PUBLISHER_SHA256,
  };
}

function statusCommand(jobId: string, afterSequence: number): WindowsUpdateBrokerCommand {
  return {
    requestId: "status-request",
    jobId,
    action: "STATUS",
    manifestUrl: "",
    signatureUrl: "",
    manifestSha256: "",
    publisherSha256: "",
    afterSequence,
  };
}

function legacyJournalFromV2(
  journal: WindowsUpdateJournalV2,
  phase: "prepared" | "assets-staged" | "services-stopped" = "prepared",
): WindowsUpdateJournalV1 {
  if (journal.currentManifest.priorRaw === null) throw new Error("fixture prior current manifest is missing");
  const prior = JSON.parse(journal.currentManifest.priorRaw) as Record<string, unknown>;
  prior.schemaVersion = 1;
  delete prior.build;
  const next: WindowsUpdateJournalV1["currentManifest"]["next"] = {
    schemaVersion: 1,
    version: journal.currentManifest.next.version,
    versionDir: journal.currentManifest.next.versionDir,
    files: journal.currentManifest.next.files,
    manifestUrl: journal.currentManifest.next.manifestUrl,
    manifestSha256: journal.currentManifest.next.manifestSha256,
    publisherSha256: journal.currentManifest.next.publisherSha256,
  };
  const healthBefore: WindowsUpdateJournalV1["healthBefore"] = {};
  if (journal.healthBefore.worker) {
    healthBefore.worker = {
      version: journal.healthBefore.worker.version,
      processEpoch: journal.healthBefore.worker.processEpoch,
      coordinatorUrl: journal.healthBefore.worker.coordinatorUrl,
    };
  }
  if (journal.healthBefore.coordinator) {
    healthBefore.coordinator = {
      version: journal.healthBefore.coordinator.version,
      processEpoch: journal.healthBefore.coordinator.processEpoch,
    };
  }
  const progress = clone(journal.progress);
  if (phase !== "prepared") {
    progress.push({
      sequence: (progress.at(-1)?.sequence ?? 0) + 1,
      at: "2026-08-16T00:00:59.000Z",
      phase,
      message: `legacy fixture reached ${phase}`,
      terminal: false,
      success: false,
    });
  }
  const { targetBuild: _targetBuild, ...journalWithoutTargetBuild } = clone(journal);
  const { path: manifestPath, signaturePath } = journalWithoutTargetBuild.signedManifest;
  const { releasePackage } = journalWithoutTargetBuild;
  if (manifestPath === null || signaturePath === null || releasePackage === null) {
    throw new Error("fixture v2 journal has no staged signed manifest or release package");
  }
  const legacy: WindowsUpdateJournalV1 = {
    ...journalWithoutTargetBuild,
    signedManifest: { ...journalWithoutTargetBuild.signedManifest, path: manifestPath, signaturePath },
    releasePackage,
    schemaVersion: 1,
    phase,
    currentManifest: {
      priorRaw: `${JSON.stringify(prior)}\n`,
      next,
    },
    healthBefore,
    stoppedRoles: phase === "services-stopped"
      ? (journal.runningBefore.worker ? ["worker"] : [])
      : [],
    progress,
  };
  assertLegacyWindowsUpdateJournal(legacy);
  return legacy;
}

function atomicWrite(path: string, contents: string): void {
  mkdirSync(dirname(path), { recursive: true });
  const temporary = `${path}.${process.pid}.test.tmp`;
  writeFileSync(temporary, contents);
  renameSync(temporary, path);
}

function clone<T>(value: T): T {
  return structuredClone(value);
}
