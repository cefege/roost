// Serves privacy-preserving email-to-tenant route resolution on loopback.
// Caddy and the standalone CLI runtime call this bounded HTTP service.
// Unknown emails receive keyed synthetic routes so existence is not disclosed.
import { Database } from "bun:sqlite";
import { createHmac } from "node:crypto";
import { closeSync, constants, existsSync, fstatSync, openSync, readFileSync } from "node:fs";
import { isAbsolute, join, resolve } from "node:path";
import { assertTenantRouteKey } from "./registry.ts";
import {
  DEFAULT_HMAC_KEY_FILE,
  DEFAULT_ROOT,
  MAX_HMAC_KEY_BYTES,
  MIN_HMAC_KEY_BYTES,
  TENANT_RESOLVER_HEALTH_PATH,
  TENANT_RESOLVER_HOST,
  TENANT_RESOLVER_MAX_BODY_BYTES,
  TENANT_RESOLVER_MAX_RATE_BUCKETS,
  TENANT_RESOLVER_ORIGIN,
  TENANT_RESOLVER_PATH,
  TENANT_RESOLVER_PORT,
  TENANT_RESOLVER_RATE_LIMIT,
  TENANT_RESOLVER_RATE_WINDOW_MS,
  type StartTenantResolverOptions,
  type TenantResolverOptions,
  type TenantResolverRuntimeConfig,
  type TenantRouteLookup,
} from "./resolver-contract.ts";
import {
  BoundedResolverRateLimiter,
  InvalidResolverRequest,
  checkedHmacKey,
  invalidRequest,
  jsonResponse,
  normalizedEmailFromBody,
  readBoundedJson,
  tenantResolverClientIp,
  unavailable,
} from "./resolver-request.ts";

export {
  TENANT_RESOLVER_HEALTH_PATH,
  TENANT_RESOLVER_HOST,
  TENANT_RESOLVER_MAX_BODY_BYTES,
  TENANT_RESOLVER_MAX_IP_HEADER_BYTES,
  TENANT_RESOLVER_MAX_RATE_BUCKETS,
  TENANT_RESOLVER_ORIGIN,
  TENANT_RESOLVER_PATH,
  TENANT_RESOLVER_PORT,
  TENANT_RESOLVER_RATE_LIMIT,
  TENANT_RESOLVER_RATE_WINDOW_MS,
} from "./resolver-contract.ts";
export type {
  StartTenantResolverOptions,
  TenantResolverOptions,
  TenantResolverRuntimeConfig,
  TenantRouteLookup,
} from "./resolver-contract.ts";
export { tenantResolverClientIp } from "./resolver-request.ts";

class ReadonlyTenantRouteLookup implements TenantRouteLookup {
  private readonly sqlite: Database;

  constructor(path: string) {
    this.sqlite = new Database(path, { readonly: true, strict: true });
  }

  getRouteKeyByEmail(email: string): string | null {
    const row = this.sqlite.query(
      "SELECT route_key FROM accounts WHERE email_normalized = ? LIMIT 1",
    ).get(email) as { route_key: string } | null;
    return row === null ? null : assertTenantRouteKey(row.route_key);
  }

  close(): void {
    this.sqlite.close(true);
  }
}

export class TenantResolver {
  private readonly hmacKey: Buffer;
  private readonly rateLimiter: BoundedResolverRateLimiter;

  constructor(private readonly options: TenantResolverOptions) {
    this.hmacKey = checkedHmacKey(options.hmacKey);
    this.rateLimiter = new BoundedResolverRateLimiter(
      options.rateLimit ?? TENANT_RESOLVER_RATE_LIMIT,
      options.rateWindowMs ?? TENANT_RESOLVER_RATE_WINDOW_MS,
      options.maxRateBuckets ?? TENANT_RESOLVER_MAX_RATE_BUCKETS,
      options.now ?? Date.now,
    );
  }

  async fetch(request: Request, peerIp: string | null): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === TENANT_RESOLVER_HEALTH_PATH && url.search === "" && request.method === "GET") {
      return jsonResponse({ ok: true }, 200);
    }
    if (url.pathname !== TENANT_RESOLVER_PATH || url.search !== "" || request.method !== "POST") {
      return jsonResponse({ error: "not found" }, 404);
    }
    if (
      request.headers.get("origin") !== TENANT_RESOLVER_ORIGIN
      || request.headers.get("content-type") !== "application/json"
      || request.headers.has("content-encoding")
    ) {
      return invalidRequest();
    }

    const clientIp = tenantResolverClientIp(request, peerIp);
    if (clientIp === null) return invalidRequest();
    const rate = this.rateLimiter.consume(clientIp);
    if (!rate.allowed) {
      return jsonResponse(
        { error: "request rejected" },
        429,
        { "retry-after": String(rate.retryAfterSeconds) },
      );
    }

    try {
      const email = normalizedEmailFromBody(await readBoundedJson(request));
      const fakeRouteKey = createHmac("sha256", this.hmacKey)
        .update("roost-tenant-route-v1\0")
        .update(email)
        .digest("hex");
      const storedRouteKey = this.options.registry.getRouteKeyByEmail(email);
      const routeKey = storedRouteKey ?? fakeRouteKey;
      assertTenantRouteKey(routeKey);
      return jsonResponse({ routeKey }, 200);
    } catch (error) {
      if (error instanceof InvalidResolverRequest) return invalidRequest();
      return unavailable();
    }
  }

  close(): void {
    this.rateLimiter.clear();
    this.hmacKey.fill(0);
  }
}

export interface TenantResolverService {
  readonly server: Bun.Server<undefined>;
  stop(): void;
}

export function startTenantResolver(options: StartTenantResolverOptions): TenantResolverService {
  const port = options.port ?? TENANT_RESOLVER_PORT;
  if (!Number.isSafeInteger(port) || port < 0 || port > 65_535) throw new RangeError("invalid tenant resolver port");
  const resolver = new TenantResolver(options);
  let stopped = false;
  const server = Bun.serve({
    hostname: TENANT_RESOLVER_HOST,
    port,
    idleTimeout: 10,
    maxRequestBodySize: TENANT_RESOLVER_MAX_BODY_BYTES,
    fetch(request, bunServer) {
      return resolver.fetch(request, bunServer.requestIP(request)?.address ?? null);
    },
    error() {
      return unavailable();
    },
  });
  return {
    server,
    stop() {
      if (stopped) return;
      stopped = true;
      server.stop(true);
      resolver.close();
    },
  };
}

export function loadTenantResolverRuntimeConfig(
  env: Record<string, string | undefined> = process.env,
): TenantResolverRuntimeConfig {
  const rootRaw = env.ROOST_SAAS_ROOT?.trim() || DEFAULT_ROOT;
  const keyFileRaw = env.ROOST_SAAS_RESOLVER_HMAC_KEY_FILE?.trim() || DEFAULT_HMAC_KEY_FILE;
  if (!isAbsolute(rootRaw)) throw new Error("ROOST_SAAS_ROOT must be an absolute path");
  if (!isAbsolute(keyFileRaw)) throw new Error("ROOST_SAAS_RESOLVER_HMAC_KEY_FILE must be an absolute path");
  const rootDir = resolve(rootRaw);
  return {
    rootDir,
    registryPath: join(rootDir, "control.db"),
    hmacKeyFile: resolve(keyFileRaw),
  };
}

export function readTenantResolverHmacKey(path: string): Buffer {
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.uid !== 0 || (stat.mode & 0o077) !== 0) {
      throw new Error("tenant resolver HMAC key file must be a root-owned, root-only regular file");
    }
    if (stat.size < MIN_HMAC_KEY_BYTES || stat.size > MAX_HMAC_KEY_BYTES) {
      throw new Error("tenant resolver HMAC key file has an invalid size");
    }
    return checkedHmacKey(readFileSync(fd));
  } finally {
    closeSync(fd);
  }
}

export async function runTenantResolver(
  env: Record<string, string | undefined> = process.env,
): Promise<void> {
  const config = loadTenantResolverRuntimeConfig(env);
  if (!existsSync(config.registryPath)) throw new Error("SaaS registry does not exist");
  const hmacKey = readTenantResolverHmacKey(config.hmacKeyFile);
  const registry = new ReadonlyTenantRouteLookup(config.registryPath);
  let service: TenantResolverService | null = null;
  try {
    service = startTenantResolver({ registry, hmacKey });
    console.log(JSON.stringify({
      event: "saas.tenant_resolver_listening",
      host: TENANT_RESOLVER_HOST,
      port: TENANT_RESOLVER_PORT,
    }));
    await new Promise<void>((resolveTermination) => {
      const terminate = () => resolveTermination();
      process.once("SIGINT", terminate);
      process.once("SIGTERM", terminate);
    });
  } finally {
    service?.stop();
    registry.close();
    hmacKey.fill(0);
  }
}
