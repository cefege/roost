// Defines tenant-resolver limits, endpoints, and dependency contracts.
// The HTTP resolver and standalone runtime share these bounded settings.
// Central constants keep public responses and abuse limits consistent.
export const TENANT_RESOLVER_HOST = "127.0.0.1";
export const TENANT_RESOLVER_PORT = 4107;
export const TENANT_RESOLVER_PATH = "/__roost/tenant/resolve";
export const TENANT_RESOLVER_HEALTH_PATH = "/healthz";
export const TENANT_RESOLVER_ORIGIN = "https://dashboard.roosttt.com";
export const TENANT_RESOLVER_MAX_BODY_BYTES = 1_024;
export const TENANT_RESOLVER_MAX_IP_HEADER_BYTES = 256;
export const TENANT_RESOLVER_RATE_LIMIT = 30;
export const TENANT_RESOLVER_RATE_WINDOW_MS = 60_000;
export const TENANT_RESOLVER_MAX_RATE_BUCKETS = 10_000;

export const DEFAULT_ROOT = "/srv/data/roost";
export const DEFAULT_HMAC_KEY_FILE = "/etc/roost/saas-resolver-hmac.key";
export const MAX_BODY_CHUNKS = 32;
export const MIN_HMAC_KEY_BYTES = 32;
export const MAX_HMAC_KEY_BYTES = 1_024;
export const JSON_HEADERS = {
  "cache-control": "no-store",
  "content-type": "application/json; charset=utf-8",
  "x-content-type-options": "nosniff",
} as const;

export interface TenantRouteLookup {
  getRouteKeyByEmail(email: string): string | null;
}

export interface TenantResolverOptions {
  registry: TenantRouteLookup;
  hmacKey: Uint8Array;
  now?: () => number;
  rateLimit?: number;
  rateWindowMs?: number;
  maxRateBuckets?: number;
}

export interface StartTenantResolverOptions extends TenantResolverOptions {
  port?: number;
}

export interface TenantResolverRuntimeConfig {
  rootDir: string;
  registryPath: string;
  hmacKeyFile: string;
}
