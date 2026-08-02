import { loadCoordConfig } from "@roost/shared/config";
import { parse as parseYaml } from "yaml";
import {
  chmodSync,
  existsSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { basename, isAbsolute, join, resolve } from "node:path";
import { installCoordAgent } from "./install-binary-agents.ts";

const PUBLIC_SERVICE = "http://127.0.0.1:4104";
const WORKER_FP = "a".repeat(64);
const ENV_KEYS = [
  "ROOST_FRONTED",
  "ROOST_PUBLIC_BIND",
  "ROOST_CF_ACCESS_TEAM_DOMAIN",
  "ROOST_CF_ACCESS_AUD",
  "ROOST_WEB_PUBLIC_URL",
] as const;

const PREREQUISITES = `cloudflared tunnel login
cloudflared tunnel create roost
cloudflared tunnel route dns roost <hostname>
# Zero Trust dashboard → Access controls → Applications → Add → Self-hosted
#   Domain: <hostname>
#   Policy: Action=Allow, Include=Emails=<your address>
#   Copy Additional settings → Application Audience (AUD) Tag
roost expose <hostname> --team <team>.cloudflareaccess.com --aud <64-hex>`;

export interface CommandResult {
  exit: number;
  stdout: string;
  stderr: string;
}

export interface ExposeDependencies {
  which(name: string): string | null;
  run(command: string[]): Promise<CommandResult> | CommandResult;
  parseYaml(source: string): unknown;
  installSource(env: Record<string, string>): Promise<void>;
  installBinary(env: Record<string, string>): Promise<void>;
  sourceInvocation: boolean;
  repoRoot: string;
  home: string;
  log(message: string): void;
  error(message: string): void;
}

interface ExposeOptions {
  hostname: string;
  team: string;
  aud: string;
  configPath: string;
}

function printPrerequisites(error: (message: string) => void): void {
  error(PREREQUISITES);
}

function usageError(message: string, deps: ExposeDependencies): null {
  deps.error(`ERROR: ${message}`);
  deps.error("Expected: expose <hostname> --team <team>.cloudflareaccess.com --aud <64-hex> [--config <path>]");
  printPrerequisites(deps.error);
  process.exitCode = 2;
  return null;
}

function parseArgs(args: string[], deps: ExposeDependencies): ExposeOptions | null {
  if (args.includes("--help") || args.includes("-h")) {
    deps.log("Usage: roost expose <hostname> --team <team>.cloudflareaccess.com --aud <64-hex> [--config <path>]");
    printPrerequisites(deps.log);
    return null;
  }
  const hostname = args[0] ?? "";
  let team = "";
  let aud = "";
  let configPath = join(deps.home, ".cloudflared", "config.yml");
  for (let index = 1; index < args.length; index += 2) {
    const flag = args[index];
    const value = args[index + 1];
    if (!value) return usageError(`${flag ?? "option"} requires a value`, deps);
    if (flag === "--team") team = value;
    else if (flag === "--aud") aud = value;
    else if (flag === "--config") configPath = value;
    else return usageError(`unknown option ${flag}`, deps);
  }
  const fqdn = /^(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z](?:[a-z0-9-]{0,61}[a-z0-9])?$/;
  if (!fqdn.test(hostname)) return usageError("hostname must be a bare FQDN without scheme, port, or path", deps);
  if (!/^[a-z0-9][a-z0-9-]*\.cloudflareaccess\.com$/.test(team)) {
    return usageError("--team must be <team>.cloudflareaccess.com", deps);
  }
  if (!/^[0-9a-f]{64}$/.test(aud)) return usageError("--aud must be exactly 64 lowercase hex characters", deps);
  return {
    hostname,
    team,
    aud,
    configPath: isAbsolute(configPath) ? configPath : resolve(configPath),
  };
}

function exactFirstIngress(document: unknown, hostname: string): boolean {
  if (!document || typeof document !== "object") return false;
  const ingress = (document as { ingress?: unknown }).ingress;
  if (!Array.isArray(ingress) || ingress.length === 0) return false;
  const first: unknown = ingress[0];
  if (!first || typeof first !== "object" || Array.isArray(first)) return false;
  const rule = first as Record<string, unknown>;
  return rule.hostname === hostname && rule.service === PUBLIC_SERVICE && !("path" in rule);
}

function expectedIngress(hostname: string): string {
  return `ingress:\n  - hostname: ${hostname}\n    service: ${PUBLIC_SERVICE}\n  - service: http_status:404`;
}
function matchesPublicServiceOutput(stdout: string): boolean {
  const lines = stdout.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  const services = lines.filter((line) => line.startsWith("service:"));
  const matchedRules = lines.filter((line) => line.startsWith("Matched rule #"));
  return services.length === 1
    && services[0] === `service: ${PUBLIC_SERVICE}`
    && (matchedRules.length === 0 || matchedRules.every((line) => line === "Matched rule #0"));
}


async function validateIngress(options: ExposeOptions, deps: ExposeDependencies): Promise<void> {
  const validate = await deps.run(["cloudflared", "--config", options.configPath, "tunnel", "ingress", "validate"]);
  if (validate.exit !== 0) throw new Error(`cloudflared ingress validation failed: ${validate.stderr.trim() || validate.stdout.trim()}`);
  let document: unknown;
  try {
    document = deps.parseYaml(readFileSync(options.configPath, "utf8"));
  } catch (error) {
    throw new Error(`invalid cloudflared YAML: ${String(error)}`);
  }
  if (!exactFirstIngress(document, options.hostname)) {
    throw new Error(`the first ingress rule must be the pathless Roost public listener rule:\n${expectedIngress(options.hostname)}`);
  }
  const urls = [
    `https://${options.hostname}/`,
    `https://${options.hostname}/api/db-export`,
    `https://${options.hostname}/internal/coord-handoff/commit`,
    `https://${options.hostname}/ws/coord-worker/${WORKER_FP}`,
  ];
  for (const url of urls) {
    const result = await deps.run(["cloudflared", "--config", options.configPath, "tunnel", "ingress", "rule", url]);
    if (result.exit !== 0 || !matchesPublicServiceOutput(result.stdout)) {
      throw new Error(`cloudflared matched the wrong service for ${url}:\n${expectedIngress(options.hostname)}`);
    }
    deps.log(`${url} → service: ${PUBLIC_SERVICE}`);
  }
}

function upsertEnvironment(source: string, values: Record<string, string>): string {
  let result = source;
  for (const key of ENV_KEYS) {
    const line = `${key}=${values[key]}`;
    const matcher = new RegExp(`^${key}=.*$`, "m");
    if (matcher.test(result)) result = result.replace(matcher, line);
    else result += `${result.length > 0 && !result.endsWith("\n") ? "\n" : ""}${line}\n`;
  }
  return result;
}

function atomicWrite(path: string, contents: string | Uint8Array, mode: number): void {
  const temporary = `${path}.roost-expose-${process.pid}`;
  writeFileSync(temporary, contents, { mode });
  chmodSync(temporary, mode);
  renameSync(temporary, path);
}

async function installFromSource(values: Record<string, string>, deps: ExposeDependencies): Promise<void> {
  const envPath = join(deps.repoRoot, ".env.local");
  const existed = existsSync(envPath);
  const previous = existed ? readFileSync(envPath) : null;
  const mode = existed ? statSync(envPath).mode & 0o777 : 0o600;
  const current = previous?.toString("utf8") ?? "";
  atomicWrite(envPath, upsertEnvironment(current, values), mode);
  try {
    await deps.installSource(values);
  } catch (error) {
    if (previous) atomicWrite(envPath, previous, mode);
    else rmSync(envPath, { force: true });
    throw error;
  }
}

function defaultDependencies(): ExposeDependencies {
  const repoRoot = resolve(import.meta.dir, "../../..");
  return {
    which: (name) => Bun.which(name),
    run: (command) => {
      const result = Bun.spawnSync(command);
      return {
        exit: result.exitCode ?? 1,
        stdout: result.stdout.toString(),
        stderr: result.stderr.toString(),
      };
    },
    parseYaml,
    installSource: async (env) => {
      const proc = Bun.spawn(["bash", "apps/coord/scripts/install.sh", "install"], {
        cwd: repoRoot,
        env: { ...process.env, ...env },
        stdio: ["ignore", "inherit", "inherit"],
      });
      if ((await proc.exited) !== 0) throw new Error("coordinator install failed");
    },
    installBinary: (env) => installCoordAgent({
      execPath: process.execPath,
      gitSha: process.env.ROOST_GIT_SHA ?? "unknown",
      env,
      log: console.log,
    }),
    sourceInvocation: basename(process.execPath) === "bun",
    repoRoot,
    home: homedir(),
    log: console.log,
    error: console.error,
  };
}

export async function expose(args: string[], overrides: Partial<ExposeDependencies> = {}): Promise<void> {
  const deps = { ...defaultDependencies(), ...overrides };
  const options = parseArgs(args, deps);
  if (!options) return;
  if (!deps.which("cloudflared")) {
    const install = process.platform === "darwin"
      ? "brew install cloudflared"
      : "https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/";
    throw new Error(`cloudflared is required: ${install}`);
  }
  if (!existsSync(options.configPath)) throw new Error(`cloudflared config not found: ${options.configPath}`);
  await validateIngress(options, deps);

  const values: Record<string, string> = {
    ROOST_FRONTED: "1",
    ROOST_PUBLIC_BIND: "127.0.0.1:4104",
    ROOST_CF_ACCESS_TEAM_DOMAIN: options.team,
    ROOST_CF_ACCESS_AUD: options.aud,
    ROOST_WEB_PUBLIC_URL: `https://${options.hostname}`,
  };
  loadCoordConfig({
    ...process.env,
    ...values,
    ROOST_TRUST_PROXY: "1",
    ROOST_COORDINATOR_BIND: process.env.ROOST_COORDINATOR_BIND ?? "127.0.0.1:4103",
  });

  if (deps.sourceInvocation) await installFromSource(values, deps);
  else await deps.installBinary(values);

  deps.log(`sudo cloudflared --config ${options.configPath} service install`);
  deps.log("# Existing service with a different config: uninstall it, then run the exact command above.");
  deps.log("# Existing service already pinned to this config: restart it.");
  deps.log(`curl -i -X POST https://${options.hostname}/roost.v1.CoordinatorService/MiscHealth`);
  deps.log("# Logged out: expect an Access login redirect. HTTP 200 means Access is absent; 530/502 means tunnel routing is wrong.");
}
