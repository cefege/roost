// Owns generated Caddy routes for active managed SaaS coordinators.
// Lifecycle reconciliation calls this router whenever tenant reachability changes.
// Atomic file replacement prevents Caddy from observing partial route configuration.
import { randomBytes } from "node:crypto";
import {
  closeSync,
  constants,
  existsSync,
  fsyncSync,
  lstatSync,
  openSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, join } from "node:path";
import type { CommandResult, CommandRunner } from "./docker.ts";
import {
  assertCanonicalUuid,
  COORDINATOR_STATES,
  coordinatorContainerName,
  type CoordinatorState,
  type RegistryCoordinator,
} from "./registry.ts";

const DEFAULT_CONF_DIR = "/srv/infra/edge/conf.d";
const TENANT_CONFIG_NAME = "roost-tenants.caddy";
const CONTAINER_CONF_DIR = "/etc/caddy/conf.d";
const BASE_CADDYFILE = "/etc/caddy/Caddyfile";
const COMMAND_OUTPUT_LIMIT = 64 * 1024;
const CURRENT_CONFIG_LIMIT = 1024 * 1024;
const SHARED_DASHBOARD_HOST = "dashboard.roosttt.com";
const LEGACY_COORDINATOR_UPSTREAM = "unix//run/roost-edge/legacy.sock";
const TENANT_RESOLVER_UPSTREAM = "unix//run/roost-edge/resolver.sock";
const AUTH_GATEWAY_UPSTREAM = "unix//run/roost-edge/auth.sock";
const AUTH_ENDPOINTS = [
  { matcher: "roost_auth_config", method: "GET", path: "/__roost/auth/config" },
  { matcher: "roost_signup_email_start", method: "POST", path: "/__roost/signup/email/start" },
  { matcher: "roost_signup_email_verify", method: "POST", path: "/__roost/signup/email/verify" },
  { matcher: "roost_auth_google_start", method: "POST", path: "/__roost/auth/google/start" },
  { matcher: "roost_auth_google_callback", method: "GET", path: "/auth/google/callback" },
  { matcher: "roost_auth_result", method: "GET", path: "/__roost/auth/result" },
  { matcher: "roost_auth_bind_device", method: "POST", path: "/__roost/auth/bind-device" },
  { matcher: "roost_auth_link_complete", method: "POST", path: "/__roost/auth/link/complete" },
] as const;
const IPV4_OCTET = "(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])";
const IPV4_PATTERN = `(?:${IPV4_OCTET}(?:[.]${IPV4_OCTET}){3})`;
const IPV6_HEXTET = "[0-9A-Fa-f]{1,4}";
const IPV6_PATTERN = "(?:"
  + `(?:${IPV6_HEXTET}:){7}${IPV6_HEXTET}`
  + `|(?:${IPV6_HEXTET}:){1,7}:`
  + `|(?:${IPV6_HEXTET}:){1,6}:${IPV6_HEXTET}`
  + `|(?:${IPV6_HEXTET}:){1,5}(?::${IPV6_HEXTET}){1,2}`
  + `|(?:${IPV6_HEXTET}:){1,4}(?::${IPV6_HEXTET}){1,3}`
  + `|(?:${IPV6_HEXTET}:){1,3}(?::${IPV6_HEXTET}){1,4}`
  + `|(?:${IPV6_HEXTET}:){1,2}(?::${IPV6_HEXTET}){1,5}`
  + `|${IPV6_HEXTET}:(?:(?::${IPV6_HEXTET}){1,6})`
  + `|:(?:(?::${IPV6_HEXTET}){1,7}|:)`
  + `|(?:(?:${IPV6_HEXTET}:){6}|::(?:${IPV6_HEXTET}:){0,5}|(?:${IPV6_HEXTET}:){1,5}:)${IPV4_PATTERN}`
  + ")";
const CONNECTING_IP_PATTERN = `^(?:${IPV4_PATTERN}|${IPV6_PATTERN})$`;
const IDENTITY_PATH = "/roost.v1.CoordinatorService/AuthCoordIdentity";
const ROUTE_KEY_RE = /^[0-9a-f]{64}$/;
const ROUTED_STATES: Partial<Record<CoordinatorState, true>> = {
  routed: true,
  invited: true,
  active: true,
};
const KNOWN_STATES = Object.fromEntries(
  COORDINATOR_STATES.map((state) => [state, true]),
) as Record<CoordinatorState, true>;

export interface CaddyTenantRouterOptions {
  confDir?: string;
  runner?: CommandRunner;
  containerName?: string;
}

interface PreviousConfig {
  bytes: Buffer;
  mode: number;
}

async function defaultRunner(argv: readonly string[]): Promise<CommandResult> {
  if (argv.length === 0 || !argv[0]) throw new Error("empty command argv");
  const child = Bun.spawn([...argv], { stdin: "ignore", stdout: "ignore", stderr: "ignore" });
  return { exitCode: await child.exited, stdout: "", stderr: "" };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function syncDirectory(path: string): void {
  const fd = openSync(path, constants.O_RDONLY | constants.O_DIRECTORY);
  try {
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
}

function writeDurableExclusive(path: string, bytes: string | Buffer, mode: number): void {
  const fd = openSync(
    path,
    constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW,
    mode,
  );
  try {
    writeFileSync(fd, bytes);
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
}

function readPreviousConfig(path: string): PreviousConfig | null {
  if (!existsSync(path)) return null;
  const stat = lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink()) {
    throw new Error("Caddy tenant route include must be a regular file");
  }
  if (stat.size > CURRENT_CONFIG_LIMIT) {
    throw new Error("Caddy tenant route include exceeded its size bound");
  }
  return { bytes: readFileSync(path), mode: stat.mode & 0o777 };
}

function assertValidRows(rows: readonly RegistryCoordinator[]): RegistryCoordinator[] {
  const seenIds = new Set<string>();
  const seenContainers = new Set<string>();
  const seenRoutedRouteKeys = new Set<string>();
  const routed: RegistryCoordinator[] = [];

  for (const row of rows) {
    if (!row || typeof row !== "object") throw new Error("invalid Caddy registry row");
    assertCanonicalUuid(row.id, "coordinator id");
    assertCanonicalUuid(row.accountId, "coordinator account id");
    if (!Number.isSafeInteger(row.ordinal) || row.ordinal < 1) {
      throw new Error("Caddy registry row has invalid coordinator ordinal");
    }
    if (typeof row.state !== "string" || !Object.hasOwn(KNOWN_STATES, row.state)) {
      throw new Error("Caddy registry row has invalid coordinator state");
    }
    if (!ROUTE_KEY_RE.test(row.routeKey)) {
      throw new Error("Caddy registry row has invalid tenant route key");
    }

    const expectedContainer = coordinatorContainerName(row.id);
    if (row.containerName !== expectedContainer) {
      throw new Error("Caddy registry row has mismatched coordinator container name");
    }
    if (seenIds.has(row.id) || seenContainers.has(row.containerName)) {
      throw new Error("Caddy registry contains duplicate coordinator routes");
    }
    seenIds.add(row.id);
    seenContainers.add(row.containerName);

    if (Object.hasOwn(ROUTED_STATES, row.state)) {
      if (seenRoutedRouteKeys.has(row.routeKey)) {
        throw new Error("Caddy registry contains a tenant route-key collision");
      }
      seenRoutedRouteKeys.add(row.routeKey);
      routed.push(row);
    }
  }

  routed.sort((left, right) => left.routeKey.localeCompare(right.routeKey));
  return routed;
}

export function renderCaddyTenantRoutes(rows: readonly RegistryCoordinator[]): string {
  const tenantHandles = assertValidRows(rows).map(
    (row) =>
      `\t\thandle_path /_roost/t/${row.routeKey}/* {\n`
      + `\t\t\treverse_proxy ${row.containerName}:4104\n`
      + "\t\t}",
  );
  const authMatchers = AUTH_ENDPOINTS.map(
    ({ matcher, method, path }) =>
      `\t@${matcher} {\n`
      + `\t\tmethod ${method}\n`
      + `\t\tpath ${path}\n`
      + `\t\theader_regexp ${matcher}_ip CF-Connecting-IP ${CONNECTING_IP_PATTERN}\n`
      + "\t}",
  );
  const authHandles = AUTH_ENDPOINTS.map(
    ({ matcher }) =>
      `\t\thandle @${matcher} {\n`
      + `\t\t\treverse_proxy ${AUTH_GATEWAY_UPSTREAM} {\n`
      + "\t\t\t\theader_up X-Forwarded-For {http.request.header.CF-Connecting-IP}\n"
      + "\t\t\t}\n"
      + "\t\t}",
  );
  const authPathMatcher = "\t@invalid_roost_auth path "
    + AUTH_ENDPOINTS.map(({ path }) => path).join(" ");
  const blocks = [
    ...authHandles,
    `\t\thandle @invalid_roost_auth {\n`
      + "\t\t\trespond \"bad request\" 400\n"
      + "\t\t}",
    `\t\thandle @prefixed_identity {\n`
      + `\t\t\trewrite * ${IDENTITY_PATH}\n`
      + `\t\t\treverse_proxy ${LEGACY_COORDINATOR_UPSTREAM}\n`
      + "\t\t}",
    `\t\thandle /__roost/tenant/resolve {\n`
      + `\t\t\treverse_proxy ${TENANT_RESOLVER_UPSTREAM} {\n`
      + "\t\t\t\theader_up X-Forwarded-For {http.request.header.CF-Connecting-IP}\n"
      + "\t\t\t}\n"
      + "\t\t}",
    ...tenantHandles,
    `\t\thandle @unknown_tenant {\n`
      + "\t\t\trewrite * {re.unknown_tenant.1}\n"
      + `\t\t\treverse_proxy ${LEGACY_COORDINATOR_UPSTREAM}\n`
      + "\t\t}",
    `\t\thandle {\n`
      + `\t\t\treverse_proxy ${LEGACY_COORDINATOR_UPSTREAM}\n`
      + "\t\t}",
  ];
  return `http://${SHARED_DASHBOARD_HOST}:8080 {\n`
    + `${authMatchers.join("\n\n")}\n\n`
    + `${authPathMatcher}\n\n`
    + "\t@prefixed_identity path_regexp prefixed_identity "
    + "^/_roost/t/[^/]+/roost[.]v1[.]CoordinatorService/AuthCoordIdentity$\n\n"
    + "\t@unknown_tenant path_regexp unknown_tenant ^/_roost/t/[^/]+(/.*)$\n\n"
    + "\troute {\n"
    + `${blocks.join("\n\n")}\n`
    + "\t}\n"
    + "}\n";
}

export class CaddyTenantRouter {
  readonly #containerName: string;
  readonly #confDir: string;
  readonly #runner: CommandRunner;

  constructor(options: CaddyTenantRouterOptions = {}) {
    this.#containerName = options.containerName ?? "caddy";
    if (!/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(this.#containerName)) {
      throw new Error("invalid Caddy container name");
    }
    this.#confDir = options.confDir ?? DEFAULT_CONF_DIR;
    this.#runner = options.runner ?? defaultRunner;
  }

  render(rows: readonly RegistryCoordinator[]): string {
    return renderCaddyTenantRoutes(rows);
  }

  async reconcile(rows: readonly RegistryCoordinator[]): Promise<void> {
    const content = this.render(rows);
    const confStat = lstatSync(this.#confDir);
    if (!confStat.isDirectory() || confStat.isSymbolicLink()) {
      throw new Error("Caddy conf.d path must be a regular directory");
    }

    const finalPath = join(this.#confDir, TENANT_CONFIG_NAME);
    const previous = readPreviousConfig(finalPath);
    const candidateName = `.roost-tenants.candidate-${randomBytes(16).toString("hex")}`;
    if (basename(candidateName).endsWith(".caddy")) {
      throw new Error("Caddy route candidate must not match the imported wildcard");
    }
    const candidatePath = join(this.#confDir, candidateName);
    const containerCandidatePath = `${CONTAINER_CONF_DIR}/${candidateName}`;

    try {
      writeDurableExclusive(candidatePath, content, 0o644);
      syncDirectory(this.#confDir);
      await this.#runChecked("Caddy validation", [
        "docker",
        "exec",
        this.#containerName,
        "caddy",
        "validate",
        "--config",
        containerCandidatePath,
        "--adapter",
        "caddyfile",
      ]);

      renameSync(candidatePath, finalPath);
      syncDirectory(this.#confDir);

      let reloadError: unknown = null;
      try {
        await this.#runChecked("Caddy reload", [
          "docker",
          "exec",
          this.#containerName,
          "caddy",
          "reload",
          "--config",
          BASE_CADDYFILE,
        ]);
      } catch (error) {
        reloadError = error;
      }
      if (reloadError === null) return;

      try {
        if (previous) {
          writeDurableExclusive(candidatePath, previous.bytes, previous.mode);
          renameSync(candidatePath, finalPath);
        } else {
          unlinkSync(finalPath);
        }
        syncDirectory(this.#confDir);
      } catch (restoreError) {
        throw new Error(
          `${errorMessage(reloadError)}; restoring prior Caddy tenant routes failed: ${errorMessage(restoreError)}`,
        );
      }

      try {
        await this.#runChecked("Caddy rollback reload", [
          "docker",
          "exec",
          this.#containerName,
          "caddy",
          "reload",
          "--config",
          BASE_CADDYFILE,
        ]);
      } catch (rollbackError) {
        throw new Error(
          `${errorMessage(reloadError)}; rollback reload failed: ${errorMessage(rollbackError)}`,
        );
      }
      throw reloadError;
    } finally {
      if (existsSync(candidatePath)) unlinkSync(candidatePath);
    }
  }

  async #runChecked(action: string, argv: readonly string[]): Promise<void> {
    const result = await this.#runner(argv);
    if (
      !result ||
      !Number.isSafeInteger(result.exitCode) ||
      typeof result.stdout !== "string" ||
      typeof result.stderr !== "string"
    ) {
      throw new Error(`${action} returned an invalid command result`);
    }
    if (
      Buffer.byteLength(result.stdout, "utf8") > COMMAND_OUTPUT_LIMIT ||
      Buffer.byteLength(result.stderr, "utf8") > COMMAND_OUTPUT_LIMIT
    ) {
      throw new Error(`${action} command output exceeded its bound`);
    }
    if (result.exitCode !== 0) {
      throw new Error(`${action} failed with exit ${result.exitCode}`);
    }
  }
}
