// Provides coordinator environment fixtures shared by the split configuration suites.
// It owns temporary secret-file creation and cleanup so tests exercise real file loading.
// Centralized profile builders keep listener and managed-container expectations aligned.

import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export const AUD = "a".repeat(64);
export const INSTANCE_ID = "11111111-1111-4111-8111-111111111111";
export const TENANT_ROUTE_KEY = "b".repeat(64);
export const MANAGED_ORIGIN = "https://dashboard.roosttt.com";
export const OUTBOX_KEY = Buffer.alloc(32, 7).toString("base64url");

const workdirs: string[] = [];

export function cleanConfigTestWorkdirs(): void {
  for (const dir of workdirs.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
}

export function createConfigTestWorkdir(prefix: string): string {
  const dir = mkdtempSync(join(tmpdir(), prefix));
  workdirs.push(dir);
  return dir;
}

export function writeSecretFiles(
  overrides: { resend?: string | Uint8Array; outbox?: string | Uint8Array } = {},
): { resendPath: string; outboxPath: string; verifyPath: string } {
  const dir = createConfigTestWorkdir("roost-config-secrets-");
  const resendPath = join(dir, "resend");
  const outboxPath = join(dir, "outbox");
  const verifyPath = join(dir, "saas-auth-verify-key");
  writeFileSync(resendPath, overrides.resend ?? "re_file_secret\n");
  writeFileSync(outboxPath, overrides.outbox ?? `${OUTBOX_KEY}\n`);
  writeFileSync(verifyPath, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITest roost-saas-auth\n");
  return { resendPath, outboxPath, verifyPath };
}

export function managedContainerEnv(
  overrides: Record<string, string | undefined> = {},
): Record<string, string | undefined> {
  const { resendPath, outboxPath, verifyPath } = writeSecretFiles();
  return {
    ROOST_MANAGED_CONTAINER: "1",
    ROOST_SAAS_MODE: "1",
    ROOST_COORDINATOR_INSTANCE_ID: INSTANCE_ID,
    ROOST_TENANT_ROUTE_KEY: TENANT_ROUTE_KEY,
    ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
    ROOST_PUBLIC_BIND: "0.0.0.0:4104",
    ROOST_TRUST_PROXY: "1",
    ROOST_WEB_PUBLIC_URL: MANAGED_ORIGIN,
    ROOST_RESEND_ENDPOINT: "https://api.resend.com/emails",
    ROOST_RESEND_API_KEY_FILE: resendPath,
    ROOST_EMAIL_FROM: "Roost <noreply@roosttt.com>",
    ROOST_EMAIL_OUTBOX_KEY_FILE: outboxPath,
    ROOST_SAAS_AUTH_VERIFY_KEY_FILE: verifyPath,
    ...overrides,
  };
}

export function publicEnv(
  overrides: Record<string, string | undefined> = {},
): Record<string, string | undefined> {
  return {
    ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
    ROOST_TRUST_PROXY: "1",
    ROOST_PUBLIC_BIND: "127.0.0.1:4104",
    ROOST_CF_ACCESS_TEAM_DOMAIN: "example.cloudflareaccess.com",
    ROOST_CF_ACCESS_AUD: AUD,
    ROOST_WEB_PUBLIC_URL: "https://roost.example.com",
    ROOST_COORDINATOR_PUBLIC_URL: "https://private.example.ts.net:4102",
    ...overrides,
  };
}

export function managedPublicEnv(
  overrides: Record<string, string | undefined> = {},
): Record<string, string | undefined> {
  return publicEnv({
    ROOST_SAAS_MODE: "1",
    ROOST_CF_ACCESS_TEAM_DOMAIN: undefined,
    ROOST_CF_ACCESS_AUD: undefined,
    ...overrides,
  });
}
