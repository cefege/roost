// Runs and verifies managed coordinator containers through the Docker CLI.
// Lifecycle and rollout operations call this runtime for every container transition.
// Container adoption is gated by the shared exact-spec contract before use.
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  AuthCoordIdentityRequestSchema,
  AuthCoordIdentityResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { chmodSync, existsSync, renameSync, rmSync } from "node:fs";
import { join } from "node:path";
import type { RegistryCoordinator } from "./registry.ts";
import { assertGoogleIdentitySubject, assertImmutableImageDigest } from "./registry.ts";
import {
  ensureInstanceLayout,
  type EnsureInstanceLayoutOptions,
  type InstanceLayout,
} from "./layout.ts";
import {
  HEALTH_POLL_MS,
  HEALTH_TIMEOUT_MS,
  SAAS_AUTH_VERIFY_KEY_CONTAINER_PATH,
  SEED_STDIN_LIMIT,
  SHA256_HEX_RE,
  TMPFS_SPEC,
  assertExactContainer,
  defaultRunner,
  envArgv,
  expectedLabels,
  labelArgv,
  mountArg,
  parseInspect,
  requiredEnvironment,
  safeCommandFailure,
  type ActivationStatus,
  type CommandRunner,
  type DockerInspect,
  type ManagedInstanceRuntimeOptions,
  type ManagedInstanceSpec,
} from "./docker-container-contract.ts";

export type {
  ActivationStatus,
  CommandResult,
  CommandRunner,
  ManagedEmailRuntimeConfig,
  ManagedInstanceRuntimeOptions,
  ManagedInstanceSpec,
} from "./docker-container-contract.ts";

export class ManagedInstanceRuntime {
  private readonly runner: CommandRunner;
  private readonly fetchImpl: typeof fetch;
  private readonly now: () => number;
  private readonly sleep: (ms: number) => Promise<void>;
  private readonly network: string;
  private readonly layoutOptions: Pick<EnsureInstanceLayoutOptions, "uid" | "gid" | "randomKey">;

  constructor(options: ManagedInstanceRuntimeOptions = {}) {
    this.runner = options.runner ?? defaultRunner;
    this.fetchImpl = options.fetchImpl ?? fetch;
    this.now = options.now ?? Date.now;
    this.sleep = options.sleep ?? ((ms) => Bun.sleep(ms));
    this.network = options.network ?? "web";
    this.layoutOptions = { uid: options.uid, gid: options.gid, randomKey: options.randomKey };
  }

  ensureLayout(spec: ManagedInstanceSpec): InstanceLayout {
    return ensureInstanceLayout(spec.account, spec.coordinator, {
      sharedResendApiKeyPath: spec.email.sharedResendApiKeyPath,
      sharedAuthVerifyKeyPath: spec.authVerifyKeyFile,
      ...this.layoutOptions,
    });
  }

  private async runSeed(
    spec: ManagedInstanceSpec,
    actionArgs: readonly string[],
    stdin?: string,
  ): Promise<void> {
    assertImmutableImageDigest(spec.coordinator.imageDigest);
    if (stdin !== undefined && Buffer.byteLength(stdin, "utf8") > SEED_STDIN_LIMIT) {
      throw new Error("managed runtime stdin exceeded its bound");
    }
    const layout = this.ensureLayout(spec);
    const environment = requiredEnvironment(spec, layout);
    const argv = [
      "docker", "run", "--rm",
      ...(stdin === undefined ? [] : ["--interactive"]),
      "--network", "none", "--user", "65532:65532",
      "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
      "--cpus", "1", "--memory", "2g", "--pids-limit", "256",
      "--tmpfs", `/tmp:${TMPFS_SPEC}`,
      "--mount", mountArg(layout.dataDir, "/data", false),
      "--mount", mountArg(layout.secretsDir, "/run/secrets", false),
      "--mount", mountArg(layout.verifierDir, "/run/auth", true),
      ...envArgv(environment),
      spec.coordinator.imageDigest,
      "__saas-instance",
      ...actionArgs,
    ];
    const result = await this.runner(argv, stdin);
    if (result.exitCode !== 0) safeCommandFailure("instance seed", result);
  }

  async seedOwnerActivation(spec: ManagedInstanceSpec): Promise<void> {
    await this.runSeed(spec, [
      "seed-owner-activation",
      "--account-id", spec.account.id,
      "--coordinator-id", spec.coordinator.id,
      "--email", spec.account.emailNormalized,
    ]);
  }

  async seedSignupGatewayOwnerActivation(
    spec: ManagedInstanceSpec,
    activationTokenHash: string,
  ): Promise<void> {
    if (!SHA256_HEX_RE.test(activationTokenHash)) {
      throw new Error("signup-gateway activation hash must be 64 lowercase hexadecimal characters");
    }
    await this.runSeed(spec, [
      "seed-signup-gateway-owner-activation",
      "--account-id", spec.account.id,
      "--coordinator-id", spec.coordinator.id,
      "--email", spec.account.emailNormalized,
    ], `${activationTokenHash}\n`);
  }

  async seedGoogleOwner(spec: ManagedInstanceSpec, googleSubject: string): Promise<void> {
    const subject = assertGoogleIdentitySubject(googleSubject);
    const payload = `${JSON.stringify({
      subject,
      emailNormalized: spec.account.emailNormalized,
    })}\n`;
    await this.runSeed(spec, [
      "seed-google-owner",
      "--account-id", spec.account.id,
      "--coordinator-id", spec.coordinator.id,
    ], payload);
  }

  private async inspect(name: string): Promise<DockerInspect | null> {
    const result = await this.runner(["docker", "inspect", name]);
    if (result.exitCode !== 0) {
      if (/No such (?:object|container)/i.test(result.stderr)) return null;
      safeCommandFailure("container inspect", result);
    }
    return parseInspect(result.stdout);
  }

  async ensureContainer(spec: ManagedInstanceSpec): Promise<void> {
    assertImmutableImageDigest(spec.coordinator.imageDigest);
    const layout = this.ensureLayout(spec);
    const existing = await this.inspect(spec.coordinator.containerName);
    if (existing) {
      assertExactContainer(existing, spec, layout, this.network);
      return;
    }
    const environment = requiredEnvironment(spec, layout);
    const argv = [
      "docker", "create", "--name", spec.coordinator.containerName,
      ...labelArgv(expectedLabels(spec)),
      "--network", this.network,
      "--user", "65532:65532", "--read-only",
      "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
      "--cpus", "1", "--memory", "2g", "--pids-limit", "256",
      "--tmpfs", `/tmp:${TMPFS_SPEC}`,
      "--log-driver", "json-file", "--log-opt", "max-size=10m", "--log-opt", "max-file=5",
      "--mount", mountArg(layout.dataDir, "/data", false),
      "--mount", mountArg(layout.secretsDir, "/run/secrets", true),
      "--mount", mountArg(layout.verifierDir, "/run/auth", true),
      ...envArgv(environment),
      spec.coordinator.imageDigest,
    ];
    const created = await this.runner(argv);
    if (created.exitCode !== 0) safeCommandFailure("container create", created);
    const adopted = await this.inspect(spec.coordinator.containerName);
    if (!adopted) throw new Error("created container could not be inspected");
    assertExactContainer(adopted, spec, layout, this.network);
  }

  async startAndVerify(spec: ManagedInstanceSpec, timeoutMs = HEALTH_TIMEOUT_MS): Promise<void> {
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 10 * 60_000) {
      throw new Error("invalid health timeout");
    }
    await this.ensureContainer(spec);
    const started = await this.runner(["docker", "start", spec.coordinator.containerName]);
    if (started.exitCode !== 0) {
      const logs = await this.runner(["docker", "logs", "--tail", "50", spec.coordinator.containerName]);
      throw new Error(`container start failed: ${logs.stderr || logs.stdout || started.stderr}`);
    }
    const deadline = this.now() + timeoutMs;
    let inspect: DockerInspect | null = null;
    while (this.now() < deadline) {
      inspect = await this.inspect(spec.coordinator.containerName);
      if (inspect?.State?.Running && inspect.State.Health?.Status === "healthy") break;
      if (inspect?.State?.Health?.Status === "unhealthy") {
        const logs = await this.runner(["docker", "logs", "--tail", "50", spec.coordinator.containerName]);
        throw new Error(`container became unhealthy: ${logs.stderr || logs.stdout || "no container logs"}`);
      }
      await this.sleep(HEALTH_POLL_MS);
    }
    if (!inspect?.State?.Running || inspect.State.Health?.Status !== "healthy") {
      throw new Error("container health check timed out");
    }
    const ip = inspect.NetworkSettings?.Networks?.[this.network]?.IPAddress;
    if (!ip) throw new Error("container has no managed network address");
    const response = await this.fetchImpl(
      `http://${ip}:4104/roost.v1.CoordinatorService/AuthCoordIdentity`,
      {
        method: "POST",
        redirect: "manual",
        headers: {
          "content-type": "application/proto",
          "connect-protocol-version": "1",
        },
        body: toBinary(AuthCoordIdentityRequestSchema, create(AuthCoordIdentityRequestSchema, {})),
        signal: AbortSignal.timeout(10_000),
      },
    );
    if (response.status !== 200 || response.type === "opaqueredirect") throw new Error("direct coordinator identity probe failed");
    const bytes = new Uint8Array(await response.arrayBuffer());
    let identity;
    try { identity = fromBinary(AuthCoordIdentityResponseSchema, bytes); }
    catch { throw new Error("direct coordinator identity response was invalid"); }
    if (!identity.saasMode || !identity.publicListener || identity.instanceId !== spec.coordinator.id) {
      throw new Error("direct coordinator identity mismatch");
    }
  }

  async stop(coordinator: RegistryCoordinator): Promise<void> {
    const inspect = await this.inspect(coordinator.containerName);
    if (!inspect?.State?.Running) return;
    const result = await this.runner(["docker", "stop", "--time", "30", coordinator.containerName]);
    if (result.exitCode !== 0) safeCommandFailure("container stop", result);
  }

  async containerHealth(coordinator: RegistryCoordinator): Promise<string> {
    const inspect = await this.inspect(coordinator.containerName);
    if (!inspect) return "missing";
    if (!inspect.State?.Running) return "stopped";
    return inspect.State.Health?.Status ?? "unknown";
  }

  async renameContainer(from: string, to: string): Promise<void> {
    const safeName = /^[a-z0-9][a-z0-9_.-]{0,127}$/;
    if (!safeName.test(from) || !safeName.test(to)) throw new Error("invalid container rename");
    const result = await this.runner(["docker", "rename", from, to]);
    if (result.exitCode !== 0) safeCommandFailure("container rename", result);
  }

  async removeContainer(name: string): Promise<void> {
    if (!/^[a-z0-9][a-z0-9_.-]{0,127}$/.test(name)) throw new Error("invalid container name");
    if (!(await this.inspect(name))) return;
    const result = await this.runner(["docker", "rm", "--force", name]);
    if (result.exitCode !== 0) safeCommandFailure("container removal", result);
  }

  async releaseOwnerActivationEmail(coordinator: RegistryCoordinator): Promise<void> {
    const result = await this.runner([
      "docker", "exec", coordinator.containerName,
      "/roost", "__saas-instance", "release-owner-activation-email",
    ]);
    if (result.exitCode !== 0) safeCommandFailure("owner activation release", result);
  }

  async recoverInterruptedReplacement(spec: ManagedInstanceSpec): Promise<void> {
    const canonicalName = spec.coordinator.containerName;
    const rollbackName = `${canonicalName}-rollback`;
    const rollbackDatabase = join(spec.coordinator.dataDir, ".roost-rollout-rollback.db");
    const rollback = await this.inspect(rollbackName);
    if (!rollback) return;
    const canonical = await this.inspect(canonicalName);
    if (canonical?.Image === spec.coordinator.imageDigest) {
      await this.removeContainer(rollbackName);
      rmSync(rollbackDatabase, { force: true });
      return;
    }
    if (rollback.Image !== spec.coordinator.imageDigest) {
      throw new Error("interrupted rollout has no container matching the registry digest");
    }
    if (canonical) await this.removeContainer(canonicalName);
    if (existsSync(rollbackDatabase)) {
      const databasePath = join(spec.coordinator.dataDir, "coordinator_v2.db");
      rmSync(`${databasePath}-wal`, { force: true });
      rmSync(`${databasePath}-shm`, { force: true });
      rmSync(databasePath, { force: true });
      renameSync(rollbackDatabase, databasePath);
      chmodSync(databasePath, 0o600);
    }
    await this.renameContainer(rollbackName, canonicalName);
  }

  async activationStatus(coordinator: RegistryCoordinator): Promise<ActivationStatus> {
    const result = await this.runner([
      "docker", "exec", coordinator.containerName,
      "/roost", "__saas-instance", "activation-status",
    ]);
    if (result.exitCode !== 0) safeCommandFailure("owner activation status", result);
    let value: unknown;
    try { value = JSON.parse(result.stdout.trim()); }
    catch { throw new Error("owner activation status response was invalid"); }
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      throw new Error("owner activation status response was invalid");
    }
    const row = value as Record<string, unknown>;
    const topology = row.credential_topology;
    if (row.event !== "saas_instance.activation_status"
      || typeof row.activated !== "boolean"
      || typeof row.expires_at_ms !== "number"
      || !Number.isSafeInteger(row.expires_at_ms)
      || (topology !== "pending-coordinator-email"
        && topology !== "pending-signup-gateway"
        && topology !== "active-native-password"
        && topology !== "active-passwordless-google"
        && topology !== "active-linked")
      || typeof row.account_id !== "string"
      || typeof row.coordinator_id !== "string"
      || row.coordinator_id !== coordinator.id) {
      throw new Error("owner activation status response was invalid");
    }
    return {
      activated: row.activated,
      expiresAtMs: row.expires_at_ms,
      accountId: row.account_id,
      coordinatorId: row.coordinator_id,
      topology,
    };
  }
}

export const managedDockerInternals = {
  TMPFS_SPEC,
  SAAS_AUTH_VERIFY_KEY_CONTAINER_PATH,
  assertExactContainer,
  requiredEnvironment,
};
