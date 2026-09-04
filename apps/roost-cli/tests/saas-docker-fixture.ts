// This fixture owns deterministic managed-container records and temporary filesystem setup.
// Docker runtime tests import its exact inspect response and command-result builders.
// Its exported cleanup keeps every test file isolated without hiding hook registration.
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { RegistryAccount, RegistryCoordinator } from "../src/saas/registry.ts";
import { managedDockerInternals, type CommandResult } from "../src/saas/docker.ts";
import { instanceLayoutFor } from "../src/saas/layout.ts";
import { writeEd25519VerificationKeyFixture } from "./managed-e2e-fixture.ts";

const ACCOUNT_ID = "11111111-1111-4111-8111-111111111111";
const COORDINATOR_ID = "22222222-2222-4222-8222-222222222222";
const IMAGE = `sha256:${"a".repeat(64)}`;
const HOSTNAME = "c-22222222222242228222222222222222.dashboard.roosttt.com";
const CONTAINER = "roost-coord-22222222222242228222222222222222";
const ROUTE_KEY = "ab".repeat(32);
const cleanups: string[] = [];

function cleanupDockerFixtures(): void {
  while (cleanups.length > 0) rmSync(cleanups.pop()!, { recursive: true, force: true });
}

interface TestSpec {
  root: string;
  account: RegistryAccount;
  coordinator: RegistryCoordinator;
  sharedKey: string;
  authVerifyKeyFile: string;
}


function testSpec(): TestSpec {
  const root = mkdtempSync(join(tmpdir(), "roost-saas-docker-"));
  cleanups.push(root);
  const sharedKey = join(root, "shared-resend-key");
  writeFileSync(sharedKey, "re_test_shared_key\n", { mode: 0o600 });
  const authVerifyKeyFile = join(root, "saas-auth-verify-key");
  writeEd25519VerificationKeyFixture(authVerifyKeyFile);
  const account: RegistryAccount = {
    id: ACCOUNT_ID,
    routeKey: ROUTE_KEY,
    emailNormalized: "owner@example.com",
    state: "pending",
    createdAtMs: 1,
    activatedAtMs: null,
    disabledAtMs: null,
  };
  const coordinator: RegistryCoordinator = {
    id: COORDINATOR_ID,
    accountId: ACCOUNT_ID,
    routeKey: ROUTE_KEY,
    ordinal: 1,
    hostname: HOSTNAME,
    containerName: CONTAINER,
    dataDir: join(root, "instances", COORDINATOR_ID, "data"),
    imageDigest: IMAGE,
    state: "reserved",
    createdAtMs: 1,
    seededAtMs: null,
    runningAtMs: null,
    routedAtMs: null,
    invitedAtMs: null,
    activatedAtMs: null,
    disabledAtMs: null,
    failedAtMs: null,
    updatedAtMs: 1,
    lastError: null,
  };
  return { root, account, coordinator, sharedKey, authVerifyKeyFile };
}

function commandOk(stdout = ""): CommandResult {
  return { exitCode: 0, stdout, stderr: "" };
}

function validInspect(spec: TestSpec) {
  const layout = instanceLayoutFor(spec.coordinator);
  const env = managedDockerInternals.requiredEnvironment({
    account: spec.account,
    coordinator: spec.coordinator,
    authVerifyKeyFile: spec.authVerifyKeyFile,
    email: {
      resendEndpoint: "https://api.resend.com/emails",
      emailFrom: "Roost <noreply@example.com>",
      sharedResendApiKeyPath: spec.sharedKey,
    },
  }, layout);
  return {
    Name: `/${CONTAINER}`,
    Image: IMAGE,
    Config: {
      Image: IMAGE,
      User: "65532:65532",
      Env: Object.entries(env).map(([name, value]) => `${name}=${value}`),
      Labels: {
        "com.roost.saas": "coordinator",
        "com.roost.account-id": ACCOUNT_ID,
        "com.roost.coordinator-id": COORDINATOR_ID,
        "com.roost.ordinal": "1",
      },
    },
    HostConfig: {
      ReadonlyRootfs: true,
      CapDrop: ["ALL"],
      SecurityOpt: ["no-new-privileges"],
      NanoCpus: 1_000_000_000,
      Memory: 2 * 1024 * 1024 * 1024,
      PidsLimit: 256,
      NetworkMode: "web",
      Tmpfs: { "/tmp": managedDockerInternals.TMPFS_SPEC },
      PortBindings: {},
      LogConfig: { Type: "json-file", Config: { "max-size": "10m", "max-file": "5" } },
    },
    Mounts: [
      { Type: "bind", Source: layout.dataDir, Destination: "/data", RW: true },
      { Type: "bind", Source: layout.secretsDir, Destination: "/run/secrets", RW: false },
      {
        Type: "bind",
        Source: layout.verifierDir,
        Destination: "/run/auth",
        RW: false,
      },
    ],
    NetworkSettings: {
      Networks: { web: { IPAddress: "172.20.0.10" } },
      Ports: { "4104/tcp": null },
    },
    State: { Running: true, Health: { Status: "healthy" } },
  };
}

export {
  CONTAINER,
  COORDINATOR_ID,
  ROUTE_KEY,
  cleanupDockerFixtures,
  commandOk,
  testSpec,
  validInspect,
};
export type { TestSpec };
