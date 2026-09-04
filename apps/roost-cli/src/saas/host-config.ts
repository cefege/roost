// Loads and validates the host-level configuration for managed SaaS services.
// CLI startup, admission, and prerequisite checks share this canonical config.
// Absolute paths and immutable digests are rejected before privileged work begins.
import { isAbsolute, join, resolve } from "node:path";
import { assertImmutableImageDigest, type SaasRegistry } from "./registry.ts";

const DEFAULT_ROOT = "/srv/data/roost";
const DEFAULT_CADDY_CONF = "/srv/infra/edge/conf.d";
const DEFAULT_CADDYFILE = "/srv/infra/edge/Caddyfile";
const DEFAULT_CLOUDFLARED_CONFIG = "/etc/cloudflared/config.yml";
const DEFAULT_CADDY_IMAGE = "caddy@sha256:5f5c8640aae01df9654968d946d8f1a56c497f1dd5c5cda4cf95ab7c14d58648";
export const BACKUP_MAX_AGE_MS = 26 * 60 * 60 * 1000;
export const ALERT_DISK_RATIO = 0.70;
export const STOP_DISK_RATIO = 0.85;
export const OUTPUT_LIMIT = 64 * 1024;
export const CADDY_EDGE_SOCKET_DIR = "/run/roost-edge";
export const AUTH_EDGE_SOCKET = "/run/roost-edge/auth.sock";
export const RESOLVER_EDGE_SOCKET = "/run/roost-edge/resolver.sock";
export const PRIVATE_PROVISION_DIR = "/run/roost-saas-private";
export const PRIVATE_PROVISION_SOCKET = `${PRIVATE_PROVISION_DIR}/provision.sock`;
export const ORIGIN_FIREWALL_TABLE = "roost_saas_origin";
export const ADMISSION_LEASE_MS = 30_000;
export const ADMISSION_LEASE_RESOURCE = "account-admission";

export interface SaasHostConfig {
  rootDir: string;
  registryPath: string;
  maxAccounts: number;
  imageDigest: string;
  network: string;
  resendEndpoint: string;
  emailFrom: string;
  sharedResendApiKeyPath: string;
  authVerifyKeyFile: string;
  ageRecipient: string;
  ageIdentityFile: string;
  caddyConfDir: string;
  caddyfilePath: string;
  cloudflaredConfigPath: string;
  caddyImageDigest: string;
}

export interface HostAdmissionOptions {
  registry: SaasRegistry;
  config: SaasHostConfig;
  now?: () => number;
  diskRatio?: () => number;
  onAlert?: (message: string) => void;
}

function required(env: Record<string, string | undefined>, name: string): string {
  const value = env[name]?.trim();
  if (!value) throw new Error(`${name} is required`);
  return value;
}
function requiredPositiveInteger(
  env: Record<string, string | undefined>,
  name: string,
): number {
  const raw = required(env, name);
  if (!/^[1-9][0-9]*$/.test(raw)) throw new Error(`${name} must be a positive integer`);
  const value = Number(raw);
  if (!Number.isSafeInteger(value)) throw new Error(`${name} must be a positive integer`);
  return value;
}

function absolutePath(value: string, name: string): string {
  if (!isAbsolute(value)) throw new Error(`${name} must be an absolute path`);
  return resolve(value);
}

export function loadSaasHostConfig(
  env: Record<string, string | undefined> = process.env,
): SaasHostConfig {
  const rootDir = absolutePath(env.ROOST_SAAS_ROOT ?? DEFAULT_ROOT, "ROOST_SAAS_ROOT");
  const imageDigest = assertImmutableImageDigest(required(env, "ROOST_SAAS_IMAGE_DIGEST"));
  const resendEndpoint = required(env, "ROOST_SAAS_RESEND_ENDPOINT");
  const endpoint = new URL(resendEndpoint);
  if (endpoint.protocol !== "https:" || endpoint.username || endpoint.password || endpoint.hash || endpoint.search) {
    throw new Error("ROOST_SAAS_RESEND_ENDPOINT must be a bare HTTPS endpoint");
  }
  const ageRecipient = required(env, "ROOST_SAAS_AGE_RECIPIENT");
  if (!/^age1[0-9a-z]{20,100}$/.test(ageRecipient)) throw new Error("ROOST_SAAS_AGE_RECIPIENT is invalid");
  return {
    rootDir,
    registryPath: join(rootDir, "control.db"),
    maxAccounts: requiredPositiveInteger(env, "ROOST_SAAS_MAX_ACCOUNTS"),
    imageDigest,
    network: env.ROOST_SAAS_DOCKER_NETWORK?.trim() || "web",
    resendEndpoint: endpoint.toString(),
    emailFrom: required(env, "ROOST_SAAS_EMAIL_FROM"),
    sharedResendApiKeyPath: absolutePath(
      required(env, "ROOST_SAAS_RESEND_API_KEY_FILE"),
      "ROOST_SAAS_RESEND_API_KEY_FILE",
    ),
    authVerifyKeyFile: absolutePath(
      required(env, "ROOST_SAAS_AUTH_VERIFY_KEY_FILE"),
      "ROOST_SAAS_AUTH_VERIFY_KEY_FILE",
    ),
    ageRecipient,
    ageIdentityFile: absolutePath(
      required(env, "ROOST_SAAS_AGE_IDENTITY_FILE"),
      "ROOST_SAAS_AGE_IDENTITY_FILE",
    ),
    caddyConfDir: absolutePath(env.ROOST_SAAS_CADDY_CONF_DIR ?? DEFAULT_CADDY_CONF, "ROOST_SAAS_CADDY_CONF_DIR"),
    caddyfilePath: absolutePath(env.ROOST_SAAS_CADDYFILE ?? DEFAULT_CADDYFILE, "ROOST_SAAS_CADDYFILE"),
    cloudflaredConfigPath: absolutePath(
      env.ROOST_SAAS_CLOUDFLARED_CONFIG ?? DEFAULT_CLOUDFLARED_CONFIG,
      "ROOST_SAAS_CLOUDFLARED_CONFIG",
    ),
    caddyImageDigest: env.ROOST_SAAS_CADDY_IMAGE_DIGEST?.trim() || DEFAULT_CADDY_IMAGE,
  };
}
