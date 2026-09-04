// Verifies managed coordinator routes, identity responses, and resolver behavior.
// Lifecycle transitions call these probes before committing reachable registry state.
// Direct and public checks ensure routing never aliases one tenant to another.
import { isIP } from "node:net";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  AuthCoordIdentityRequestSchema,
  AuthCoordIdentityResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { AUTH_LAYER_DEVICE, X_ROOST_AUTH_LAYER } from "@roost/shared/wire/headers";
import type { CommandResult, CommandRunner } from "./docker.ts";
import type { RegistryAccount, RegistryCoordinator } from "./registry.ts";
import { TENANT_RESOLVER_PATH } from "./resolver.ts";

const IDENTITY_PATH = "/roost.v1.CoordinatorService/AuthCoordIdentity";
const PROBE_TIMEOUT_MS = 10_000;
const SHARED_PUBLIC_ORIGIN = "https://dashboard.roosttt.com";
const SHARED_PUBLIC_HOST = "dashboard.roosttt.com";
const TENANT_ROUTE_PREFIX = "/_roost/t";
const FAKE_IDENTITY_ROUTE_KEY = "not-a-tenant";
const ROUTE_KEY_RE = /^[0-9a-f]{64}$/;
const COMMAND_OUTPUT_LIMIT = 64 * 1024;
const IDENTITY_REQUEST_BODY = toBinary(
  AuthCoordIdentityRequestSchema,
  create(AuthCoordIdentityRequestSchema, {}),
);
const SENSITIVE_PATHS = [
  "/api/db-export",
  "/roost.v1.CoordinatorService/CoordinatorMovePreflight",
  "/roost.v1.CoordinatorService/CoordinatorMoveStart",
  "/roost.v1.CoordinatorService/CoordinatorMoveStatus",
  "/roost.v1.CoordinatorService/WorkersDeployStart",
  "/roost.v1.CoordinatorService/WorkersDeployOutput",
  "/roost.v1.CoordinatorService/DiagSnapshot",
  "/roost.v1.CoordinatorService/DefinitelyUnknown",
] as const;

export interface ManagedRouteProbeOptions {
  fetchImpl?: typeof fetch;
  localOrigin?: string;
  runner?: CommandRunner;
}

interface IdentityProbeResponse {
  status: number;
  body: Uint8Array;
  accessControlAllowOrigin: string | null;
  contentType: string | null;
  contentTypeOptions: string | null;
}

interface DirectDockerInspect {
  Name?: string;
  State?: { Running?: boolean };
  NetworkSettings?: {
    Networks?: Record<string, { IPAddress?: string }>;
  };
}

async function defaultRunner(argv: readonly string[]): Promise<CommandResult> {
  const child = Bun.spawn([...argv], { stdin: "ignore", stdout: "pipe", stderr: "pipe" });
  let timedOut = false;
  const timeout = setTimeout(() => {
    timedOut = true;
    child.kill();
  }, PROBE_TIMEOUT_MS);
  timeout.unref();
  const [exitCode, stdout, stderr] = await Promise.all([
    child.exited,
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
  ]).finally(() => clearTimeout(timeout));
  if (timedOut) throw new Error("direct tenant identity inspection timed out");
  return { exitCode, stdout, stderr };
}

export class ManagedRouteProbe {
  private readonly fetchImpl: typeof fetch;
  private readonly localOrigin: string;
  private readonly runner: CommandRunner;

  constructor(options: ManagedRouteProbeOptions = {}) {
    this.fetchImpl = options.fetchImpl ?? fetch;
    this.localOrigin = options.localOrigin ?? "http://127.0.0.1:8080";
    this.runner = options.runner ?? defaultRunner;
  }

  private tenantPath(coordinator: RegistryCoordinator, path: string): string {
    if (!ROUTE_KEY_RE.test(coordinator.routeKey)) {
      throw new Error("tenant route probe received an invalid route key");
    }
    if (!path.startsWith("/")) throw new Error("tenant route probe path must be absolute");
    return `${TENANT_ROUTE_PREFIX}/${coordinator.routeKey}${path}`;
  }

  private headers(local: boolean, values?: HeadersInit): Headers {
    const headers = new Headers(values);
    if (local) headers.set("host", SHARED_PUBLIC_HOST);
    return headers;
  }

  private async directContainerOrigin(coordinator: RegistryCoordinator): Promise<string> {
    const result = await this.runner(["docker", "inspect", coordinator.containerName]);
    if (!result
      || !Number.isSafeInteger(result.exitCode)
      || typeof result.stdout !== "string"
      || typeof result.stderr !== "string") {
      throw new Error("direct tenant identity inspection returned an invalid command result");
    }
    if (Buffer.byteLength(result.stdout) > COMMAND_OUTPUT_LIMIT
      || Buffer.byteLength(result.stderr) > COMMAND_OUTPUT_LIMIT) {
      throw new Error("direct tenant identity inspection output exceeded its bound");
    }
    if (result.exitCode !== 0) throw new Error("direct tenant identity inspection failed");

    let parsed: unknown;
    try {
      parsed = JSON.parse(result.stdout);
    } catch {
      throw new Error("direct tenant identity inspection returned invalid JSON");
    }
    if (!Array.isArray(parsed) || parsed.length !== 1 || !parsed[0] || typeof parsed[0] !== "object") {
      throw new Error("direct tenant identity inspection returned an unexpected shape");
    }
    const inspect = parsed[0] as DirectDockerInspect;
    if (inspect.Name !== `/${coordinator.containerName}` || !inspect.State?.Running) {
      throw new Error("direct tenant identity container mismatch");
    }
    const networks = Object.values(inspect.NetworkSettings?.Networks ?? {});
    if (networks.length !== 1) throw new Error("direct tenant identity network mismatch");
    const address = networks[0]?.IPAddress;
    const addressKind = typeof address === "string" ? isIP(address) : 0;
    if (addressKind === 0) throw new Error("direct tenant identity network address is invalid");
    return `http://${addressKind === 6 ? `[${address}]` : address}:4104`;
  }

  private async assertDirectIdentity(coordinator: RegistryCoordinator): Promise<void> {
    const origin = await this.directContainerOrigin(coordinator);
    const response = await this.fetchImpl(`${origin}${IDENTITY_PATH}`, {
      method: "POST",
      redirect: "manual",
      headers: {
        "content-type": "application/proto",
        "connect-protocol-version": "1",
      },
      body: IDENTITY_REQUEST_BODY,
      signal: AbortSignal.timeout(PROBE_TIMEOUT_MS),
    });
    if (response.status !== 200 || response.type === "opaqueredirect") {
      throw new Error("direct tenant identity probe failed");
    }
    let identity;
    try {
      identity = fromBinary(
        AuthCoordIdentityResponseSchema,
        new Uint8Array(await response.arrayBuffer()),
      );
    } catch {
      throw new Error("direct tenant identity response was malformed");
    }
    if (!identity.saasMode
      || !identity.publicListener
      || identity.instanceId !== coordinator.id) {
      throw new Error("direct tenant identity mismatch");
    }
  }

  private async sharedIdentityResponse(
    origin: string,
    routeKey: string | null,
    local: boolean,
  ): Promise<IdentityProbeResponse> {
    const path = routeKey === null
      ? IDENTITY_PATH
      : `${TENANT_ROUTE_PREFIX}/${routeKey}${IDENTITY_PATH}`;
    const response = await this.fetchImpl(`${origin}${path}`, {
      method: "POST",
      redirect: "manual",
      headers: this.headers(local, {
        "content-type": "application/proto",
        "connect-protocol-version": "1",
        origin: "https://attacker.invalid",
      }),
      body: IDENTITY_REQUEST_BODY,
      signal: AbortSignal.timeout(PROBE_TIMEOUT_MS),
    });
    if (response.type === "opaqueredirect") {
      throw new Error(`${local ? "local" : "public"} shared identity probe redirected`);
    }
    return {
      status: response.status,
      body: new Uint8Array(await response.arrayBuffer()),
      accessControlAllowOrigin: response.headers.get("access-control-allow-origin"),
      contentType: response.headers.get("content-type"),
      contentTypeOptions: response.headers.get("x-content-type-options"),
    };
  }

  private async assertSharedIdentity(
    origin: string,
    coordinator: RegistryCoordinator,
    local: boolean,
  ): Promise<void> {
    if (!ROUTE_KEY_RE.test(coordinator.routeKey)) {
      throw new Error("tenant route probe received an invalid route key");
    }
    const [legacy, known, fake] = await Promise.all([
      this.sharedIdentityResponse(origin, null, local),
      this.sharedIdentityResponse(origin, coordinator.routeKey, local),
      this.sharedIdentityResponse(origin, FAKE_IDENTITY_ROUTE_KEY, local),
    ]);
    if (legacy.status !== 200
      || legacy.accessControlAllowOrigin !== null
      || legacy.contentTypeOptions !== "nosniff") {
      throw new Error(`${local ? "local" : "public"} shared legacy identity response was invalid`);
    }
    for (const candidate of [known, fake]) {
      if (candidate.status !== legacy.status
        || candidate.accessControlAllowOrigin !== legacy.accessControlAllowOrigin
        || candidate.contentType !== legacy.contentType
        || candidate.contentTypeOptions !== legacy.contentTypeOptions
        || !Buffer.from(candidate.body).equals(Buffer.from(legacy.body))) {
        throw new Error(
          `${local ? "local" : "public"} prefixed identity response exposed tenant routing`,
        );
      }
    }
  }

  private async status(
    origin: string,
    coordinator: RegistryCoordinator,
    path: string,
    expectedStatus: number,
    local: boolean,
    expectedAuthLayer = false,
  ): Promise<void> {
    const response = await this.fetchImpl(`${origin}${this.tenantPath(coordinator, path)}`, {
      method: path.startsWith("/roost.") ? "POST" : "GET",
      redirect: "manual",
      headers: this.headers(local, { origin: "https://attacker.invalid" }),
      signal: AbortSignal.timeout(PROBE_TIMEOUT_MS),
    });
    if (response.status !== expectedStatus || response.type === "opaqueredirect") {
      throw new Error(`tenant sensitive-route probe failed for ${path}`);
    }
    if (response.headers.get("access-control-allow-origin") !== null) {
      throw new Error("tenant route emitted attacker ACAO");
    }
    if (expectedAuthLayer && response.headers.get(X_ROOST_AUTH_LAYER) !== AUTH_LAYER_DEVICE) {
      throw new Error("protected tenant route did not reject at the device layer");
    }
  }

  async verifyResolver(account: RegistryAccount): Promise<void> {
    if (!ROUTE_KEY_RE.test(account.routeKey) || account.emailNormalized.length === 0) {
      throw new Error("shared resolver probe received an invalid account");
    }
    const origins: Array<{ origin: string; local: boolean }> = [
      { origin: this.localOrigin, local: true },
      { origin: SHARED_PUBLIC_ORIGIN, local: false },
    ];
    for (const target of origins) {
      const response = await this.fetchImpl(`${target.origin}${TENANT_RESOLVER_PATH}`, {
        method: "POST",
        redirect: "manual",
        headers: this.headers(target.local, {
          "content-type": "application/json",
          origin: SHARED_PUBLIC_ORIGIN,
          "cf-connecting-ip": "127.0.0.1",
        }),
        body: JSON.stringify({ email: account.emailNormalized }),
        signal: AbortSignal.timeout(PROBE_TIMEOUT_MS),
      });
      if (response.status !== 200
        || response.type === "opaqueredirect"
        || response.headers.get("access-control-allow-origin") !== null
        || response.headers.get("cache-control") !== "no-store"
        || response.headers.get("content-type") !== "application/json; charset=utf-8"
        || response.headers.get("x-content-type-options") !== "nosniff") {
        throw new Error(`${target.local ? "local" : "public"} shared resolver probe failed`);
      }
      let body: unknown;
      try {
        body = await response.json();
      } catch {
        throw new Error(`${target.local ? "local" : "public"} shared resolver response was malformed`);
      }
      if (!body
        || typeof body !== "object"
        || Array.isArray(body)
        || !("routeKey" in body)
        || Object.keys(body).length !== 1
        || body.routeKey !== account.routeKey) {
        throw new Error(`${target.local ? "local" : "public"} shared resolver route mismatch`);
      }
    }
  }

  async verify(coordinator: RegistryCoordinator): Promise<void> {
    await this.assertDirectIdentity(coordinator);
    const origins: Array<{ origin: string; local: boolean }> = [
      { origin: this.localOrigin, local: true },
      { origin: SHARED_PUBLIC_ORIGIN, local: false },
    ];
    for (const target of origins) {
      await this.assertSharedIdentity(target.origin, coordinator, target.local);
      await this.status(
        target.origin,
        coordinator,
        "/roost.v1.CoordinatorService/SessionsList",
        401,
        target.local,
        true,
      );
      for (const path of SENSITIVE_PATHS) {
        await this.status(target.origin, coordinator, path, 404, target.local);
      }
    }
  }
}

export const managedProbeContract = {
  identityPath: IDENTITY_PATH,
  resolverPath: TENANT_RESOLVER_PATH,
  deviceAuthHeader: X_ROOST_AUTH_LAYER,
  sensitivePaths: SENSITIVE_PATHS,
};
