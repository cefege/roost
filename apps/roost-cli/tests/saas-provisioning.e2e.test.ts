// These E2Es prove tenant isolation and routed lifecycle behavior across real containers.
// The managed profile supplies one immutable coordinator image and one shared Docker network.
// Each scenario owns only the coordinator and Caddy containers plus its temporary host data.

import { expect, test } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { Database } from "bun:sqlite";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createEmailOutboxPayloadCipher } from "@roost/shared/email-payload";
import { fingerprintOf } from "@roost/shared/fingerprint";
import {
  AuthMintBootstrapRequestSchema,
  AuthMintBootstrapResponseSchema,
  AuthOwnerActivateRequestSchema,
  AuthOwnerActivateResponseSchema,
  AuthRedeemWorkerRequestSchema,
  WorkersListRequestSchema,
  WorkersListResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { CaddyTenantRouter } from "../src/saas/caddy.ts";
import { SaasLifecycle, type TenantRouteManager } from "../src/saas/lifecycle.ts";
import { SaasRegistry } from "../src/saas/registry.ts";
import { ManagedInstanceRuntime, type ManagedInstanceSpec } from "../src/saas/docker.ts";
import type { RegistryAccount, RegistryCoordinator } from "../src/saas/registry.ts";
import { runSignupInit } from "../src/saas-auth/signup-init.ts";
import { instanceLayoutFor } from "../src/saas/layout.ts";
import { requiredManagedE2eResources } from "./managed-e2e-fixture.ts";

const enabled = process.env.ROOST_SAAS_E2E === "1";
const ACCOUNTS = [
  {
    accountId: "11111111-1111-4111-8111-111111111111",
    coordinatorId: "22222222-2222-4222-8222-222222222222",
    email: "owner-a@example.com",
  },
  {
    accountId: "33333333-3333-4333-8333-333333333333",
    coordinatorId: "44444444-4444-4444-8444-444444444444",
    email: "owner-b@example.com",
  },
] as const;

function docker(args: readonly string[]): string {
  const result = Bun.spawnSync(["docker", ...args], { stdout: "pipe", stderr: "pipe" });
  if (result.exitCode !== 0) throw new Error(`docker ${args[0] ?? "command"} failed`);
  return result.stdout.toString().trim();
}

function routeKey(accountId: string): string {
  return accountId.replaceAll("-", "").repeat(2);
}

function coordinator(
  accountId: string,
  coordinatorId: string,
  root: string,
  imageId: string,
): RegistryCoordinator {
  const hex = coordinatorId.replaceAll("-", "");
  return {
    id: coordinatorId,
    accountId,
    routeKey: routeKey(accountId),
    ordinal: 1,
    hostname: `c-${hex}.dashboard.roosttt.com`,
    containerName: `roost-coord-${hex}`,
    dataDir: join(root, "instances", coordinatorId, "data"),
    imageDigest: imageId,
    state: "seeded",
    createdAtMs: 1,
    seededAtMs: 1,
    runningAtMs: null,
    routedAtMs: null,
    invitedAtMs: null,
    activatedAtMs: null,
    disabledAtMs: null,
    failedAtMs: null,
    updatedAtMs: 1,
    lastError: null,
  };
}

function account(id: string, email: string): RegistryAccount {
  return {
    id,
    routeKey: routeKey(id),
    emailNormalized: email,
    state: "pending",
    createdAtMs: 1,
    activatedAtMs: null,
    disabledAtMs: null,
  };
}

interface BrowserIdentity {
  pair: CryptoKeyPair;
  publicKey: Uint8Array;
  fingerprint: string;
}


async function keyPair(): Promise<BrowserIdentity> {
  const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"]);
  const publicKey = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  return { pair, publicKey, fingerprint: await fingerprintOf(publicKey) };
}

function b64url(value: string | Uint8Array): string {
  return Buffer.from(typeof value === "string" ? new TextEncoder().encode(value) : value).toString("base64url");
}

async function jwt(identity: BrowserIdentity): Promise<string> {
  const now = Math.floor(Date.now() / 1000);
  const header = b64url(JSON.stringify({ alg: "EdDSA", typ: "JWT", kid: identity.fingerprint }));
  const payload = b64url(JSON.stringify({
    sub: identity.fingerprint,
    aud: "roost-coordinator",
    iat: now,
    exp: now + 300,
  }));
  const message = new TextEncoder().encode(`${header}.${payload}`);
  const signature = new Uint8Array(await crypto.subtle.sign("Ed25519", identity.pair.privateKey, message));
  return `${header}.${payload}.${b64url(signature)}`;
}

function containerIp(name: string, network: string): string {
  return docker(["inspect", name, "--format", `{{(index .NetworkSettings.Networks "${network}").IPAddress}}`]);
}

async function rpc(
  base: string,
  method: string,
  body: Uint8Array<ArrayBuffer>,
  auth?: { jwt: string; dashboardId: string },
): Promise<Uint8Array> {
  const headers = new Headers({
    "content-type": "application/proto",
    "connect-protocol-version": "1",
  });
  if (auth) {
    headers.set("authorization", `Bearer ${auth.jwt}`);
    headers.set("x-roost-dashboard-id", auth.dashboardId);
  }
  const response = await fetch(`${base}/roost.v1.CoordinatorService/${method}`, {
    method: "POST",
    headers,
    body,
  });
  if (response.status !== 200) throw new Error(`${method} failed with ${response.status}`);
  return new Uint8Array(await response.arrayBuffer());
}

function activationToken(spec: ManagedInstanceSpec): string {
  const layout = instanceLayoutFor(spec.coordinator);
  const sqlite = new Database(join(layout.dataDir, "coordinator_v2.db"), { readonly: true });
  try {
    const row = sqlite.query(
      "SELECT id, kind, encrypted_payload FROM email_outbox WHERE kind = 'owner_activation'",
    ).get() as { id: string; kind: string; encrypted_payload: string };
    const cipher = createEmailOutboxPayloadCipher(readFileSync(layout.outboxKeyPath, "utf8"));
    const payload = cipher.decrypt({ outboxId: row.id, kind: row.kind }, row.encrypted_payload);
    const match = new RegExp(`/activate/${spec.coordinator.routeKey}#([A-Za-z0-9_-]+)`).exec(payload.text ?? payload.html);
    if (!match?.[1]) throw new Error("activation token was absent from encrypted outbox payload");
    return match[1];
  } finally {
    sqlite.close(true);
  }
}

test.skipIf(!enabled)("provisions and isolates two disposable managed coordinators", async () => {
  if (typeof process.getuid !== "function" || process.getuid() !== 0) {
    throw new Error("ROOST_SAAS_E2E requires Linux root");
  }
  const { imageId, network } = requiredManagedE2eResources();
  const root = mkdtempSync(join(tmpdir(), "roost-saas-e2e-"));
  const sharedKey = join(root, "resend-api-key");
  writeFileSync(sharedKey, "re_e2e_not_delivered", { mode: 0o600 });
  const initialized = runSignupInit({ credentialDirectory: join(root, "saas-auth") });
  const authVerifyKeyFile = initialized.assertionVerifyKeyPath;
  expect(statSync(authVerifyKeyFile).mode & 0o777).toBe(0o644);
  expect(statSync(initialized.assertionSigningKeyPath).mode & 0o777).toBe(0o600);
  const specs = ACCOUNTS.map((fixture) => ({
    account: account(fixture.accountId, fixture.email),
    coordinator: coordinator(fixture.accountId, fixture.coordinatorId, root, imageId),
    authVerifyKeyFile,
    email: {
      resendEndpoint: "https://api.resend.com/emails",
      emailFrom: "Roost E2E <noreply@example.com>",
      sharedResendApiKeyPath: sharedKey,
    },
  } satisfies ManagedInstanceSpec));
  const runtime = new ManagedInstanceRuntime({ network });
  try {
    await Promise.all(specs.map((spec) => runtime.seedOwnerActivation(spec)));
    const tokens = specs.map(activationToken);
    await Promise.all(specs.map((spec) => runtime.startAndVerify(spec, 120_000)));

    const browserKeys = await Promise.all(specs.map(() => keyPair()));
    const dashboardIds: string[] = [];
    for (let index = 0; index < specs.length; index++) {
      const spec = specs[index]!;
      const identity = browserKeys[index]!;
      const response = await rpc(
        `http://${containerIp(spec.coordinator.containerName, network)}:4104`,
        "AuthOwnerActivate",
        toBinary(AuthOwnerActivateRequestSchema, create(AuthOwnerActivateRequestSchema, {
          token: tokens[index]!,
          newPassword: "correct horse battery staple",
          sshPubkeyB64: Buffer.from(identity.publicKey).toString("base64"),
          label: `E2E browser ${index + 1}`,
        })),
      );
      dashboardIds.push(fromBinary(AuthOwnerActivateResponseSchema, response).dashboardId);
    }
    expect(dashboardIds).toEqual(specs.map((spec) => spec.coordinator.id));
    expect(new Set(specs.map((spec) => spec.account.id)).size).toBe(2);
    expect(new Set(specs.map((spec) => spec.coordinator.routeKey)).size).toBe(2);
    expect(new Set(specs.map((spec) => spec.coordinator.containerName)).size).toBe(2);
    expect(new Set(specs.map((spec) => spec.coordinator.dataDir)).size).toBe(2);
    expect(new Set(specs.map((spec) => readFileSync(instanceLayoutFor(spec.coordinator).outboxKeyPath, "utf8"))).size).toBe(2);
    expect(new Set(specs.map((spec) => readFileSync(instanceLayoutFor(spec.coordinator).coordinatorKeyPath, "utf8"))).size).toBe(2);

    const browserJwtA = await jwt(browserKeys[0]!);
    const mintBytes = await rpc(
      `http://${containerIp(specs[0]!.coordinator.containerName, network)}:4104`,
      "AuthMintBootstrap",
      toBinary(AuthMintBootstrapRequestSchema, create(AuthMintBootstrapRequestSchema, {
        kind: "worker",
        label: "worker-a",
      })),
      { jwt: browserJwtA, dashboardId: specs[0]!.coordinator.id },
    );
    const bootstrap = fromBinary(AuthMintBootstrapResponseSchema, mintBytes).token;
    const worker = await keyPair();
    await rpc(
      `http://${containerIp(specs[0]!.coordinator.containerName, network)}:4104`,
      "AuthRedeemWorker",
      toBinary(AuthRedeemWorkerRequestSchema, create(AuthRedeemWorkerRequestSchema, {
        token: bootstrap,
        sshPubkeyB64: Buffer.from(worker.publicKey).toString("base64"),
        label: "worker-a",
        os: "linux",
      })),
    );
    const lists = await Promise.all(specs.map(async (spec, index) => fromBinary(
      WorkersListResponseSchema,
      await rpc(
        `http://${containerIp(spec.coordinator.containerName, network)}:4104`,
        "WorkersList",
        toBinary(WorkersListRequestSchema, create(WorkersListRequestSchema, {})),
        { jwt: await jwt(browserKeys[index]!), dashboardId: spec.coordinator.id },
      ),
    )));
    expect(lists[0]!.workers).toHaveLength(1);
    expect(lists[1]!.workers).toHaveLength(0);
  } finally {
    for (const spec of specs) {
      Bun.spawnSync(["docker", "rm", "--force", spec.coordinator.containerName], { stdout: "ignore", stderr: "ignore" });
    }
    rmSync(root, { recursive: true, force: true });
  }
}, 180_000);
test.skipIf(!enabled)("drives concurrent registry lifecycle through disposable Caddy and held-mail release", async () => {
  if (typeof process.getuid !== "function" || process.getuid() !== 0) {
    throw new Error("ROOST_SAAS_E2E requires Linux root");
  }
  const { imageId, network } = requiredManagedE2eResources();
  const root = mkdtempSync(join(tmpdir(), "roost-saas-lifecycle-e2e-"));
  const suffix = root.slice(-12).replaceAll(/[^a-zA-Z0-9]/g, "");
  const caddyName = `roost-caddy-${suffix}`;
  const edgeDir = join(root, "edge");
  const confDir = join(edgeDir, "conf.d");
  const caddyFile = join(edgeDir, "Caddyfile");
  const sharedKey = join(root, "resend-api-key");
  mkdirSync(confDir, { recursive: true, mode: 0o755 });
  writeFileSync(
    caddyFile,
    "{\n\tadmin 127.0.0.1:2019\n\tauto_https off\n}\n\nimport /etc/caddy/conf.d/*.caddy\n",
    { mode: 0o644 },
  );
  writeFileSync(join(confDir, "roost-tenants.caddy"), ":8080 {\n\trespond 404\n}\n", { mode: 0o644 });
  writeFileSync(sharedKey, "re_lifecycle_not_delivered", { mode: 0o600 });
  const { assertionVerifyKeyPath: authVerifyKeyFile } = runSignupInit({
    credentialDirectory: join(root, "saas-auth"),
  });
  docker([
    "run",
    "--detach",
    "--name",
    caddyName,
    "--network",
    network,
    "--volume",
    `${caddyFile}:/etc/caddy/Caddyfile:ro`,
    "--volume",
    `${confDir}:/etc/caddy/conf.d:ro`,
    "caddy@sha256:5f5c8640aae01df9654968d946d8f1a56c497f1dd5c5cda4cf95ab7c14d58648",
  ]);
  const registry = new SaasRegistry({ rootDir: root, path: join(root, "control.db") });
  const runtime = new ManagedInstanceRuntime({ network });
  const caddy = new CaddyTenantRouter({ confDir, containerName: caddyName });
  const routes: TenantRouteManager = {
    reconcile: (coordinators) => caddy.reconcile(coordinators),
    verify: async (coordinator) => {
      const response = await fetch(
        `http://${containerIp(caddyName, network)}:8080/_roost/t/${coordinator.routeKey}/roost.v1.CoordinatorService/SessionsList`,
        {
          method: "POST",
          headers: {
            host: "dashboard.roosttt.com",
            "content-type": "application/proto",
            "connect-protocol-version": "1",
          },
          body: new Uint8Array(),
        },
      );
      if (response.status !== 401) throw new Error("disposable Caddy lifecycle verification failed");
    },
    verifyResolver: async () => {},
  };
  const lifecycle = new SaasLifecycle({
    registry,
    runtime,
    routes,
    admission: {
      assertBeforeReservation: async (reserve) => reserve(),
      assertPendingWorkAllowed: async () => {},
    },
    email: {
      resendEndpoint: "https://api.resend.com/emails",
      emailFrom: "Roost E2E <noreply@example.com>",
      sharedResendApiKeyPath: sharedKey,
    },
    authVerifyKeyFile,
  });
  try {
    const results = await Promise.all([
      lifecycle.accountCreate("concurrent-a@example.com", imageId),
      lifecycle.accountCreate("concurrent-b@example.com", imageId),
    ]);
    const coordinators = results.map((result) => result.coordinator);
    expect(coordinators.map((coordinator) => coordinator.state)).toEqual(["invited", "invited"]);
    expect(registry.listAccounts()).toHaveLength(2);
    expect(registry.listCoordinators()).toHaveLength(2);
    const include = readFileSync(join(confDir, "roost-tenants.caddy"), "utf8");
    expect(include.match(/http:\/\/dashboard[.]roosttt[.]com:8080/g)).toHaveLength(1);
    for (const coordinator of coordinators) {
      expect(include).toContain(`handle_path /_roost/t/${coordinator.routeKey}/*`);
      expect(include).toContain(`reverse_proxy ${coordinator.containerName}:4104`);
      const layout = instanceLayoutFor(coordinator);
      const sqlite = new Database(join(layout.dataDir, "coordinator_v2.db"), { readonly: true });
      try {
        const outbox = sqlite.query(
          "SELECT next_attempt_ms FROM email_outbox WHERE kind = 'owner_activation'",
        ).get() as { next_attempt_ms: number };
        expect(outbox.next_attempt_ms).toBeLessThan(Number.MAX_SAFE_INTEGER);
      } finally {
        sqlite.close(true);
      }
    }
  } finally {
    for (const coordinator of registry.listCoordinators()) {
      Bun.spawnSync(["docker", "rm", "--force", coordinator.containerName], { stdout: "ignore", stderr: "ignore" });
    }
    registry.close();
    Bun.spawnSync(["docker", "rm", "--force", caddyName], { stdout: "ignore", stderr: "ignore" });
    rmSync(root, { recursive: true, force: true });
  }
}, 180_000);
