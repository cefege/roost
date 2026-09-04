/**
 * Loads the signup gateway's fixed network endpoints and root-managed credentials.
 * Gateway startup depends on this module before constructing auth or provisioning services.
 * Strict file ownership and endpoint checks keep secrets local and outbound traffic allowlisted.
 */

import { constants, closeSync, fstatSync, openSync, readFileSync } from "node:fs";
import { createPrivateKey } from "node:crypto";
import { isAbsolute, join } from "node:path";

export const GATEWAY_PUBLIC_ORIGIN = "https://dashboard.roosttt.com" as const;
export const GATEWAY_LISTEN_HOST = "127.0.0.1" as const;
export const GATEWAY_LISTEN_PORT = 4108 as const;
export const GATEWAY_STATE_DATABASE = "/var/lib/roost-signup/auth.db" as const;
export const SIGNUP_EMAIL_FROM = "Roost <noreply@roosttt.com>" as const;

// No caller-supplied URL is accepted anywhere in gateway configuration.
export const GATEWAY_OUTBOUND_ENDPOINTS = Object.freeze({
  googleAuthorization: "https://accounts.google.com/o/oauth2/v2/auth",
  googleToken: "https://oauth2.googleapis.com/token",
  googleJwks: "https://www.googleapis.com/oauth2/v3/certs",
  turnstileSiteverify: "https://challenges.cloudflare.com/turnstile/v0/siteverify",
  resendEmails: "https://api.resend.com/emails",
} as const);

export const GATEWAY_CREDENTIAL_NAMES = [
  "google-client-secret",
  "turnstile-secret",
  "resend-api-key",
  "email-outbox-key",
  "oauth-state-key",
  "assertion-signing-key",
] as const;

export type GatewayCredentialName = typeof GATEWAY_CREDENTIAL_NAMES[number];

export interface GatewayConfig {
  signupEnabled: boolean;
  googleEnabled: boolean;
  googleClientId: string;
  turnstileSiteKey: string;
  emailFrom: typeof SIGNUP_EMAIL_FROM;
  publicOrigin: typeof GATEWAY_PUBLIC_ORIGIN;
  listenHost: typeof GATEWAY_LISTEN_HOST;
  listenPort: typeof GATEWAY_LISTEN_PORT;
  stateDatabasePath: typeof GATEWAY_STATE_DATABASE;
  outbound: typeof GATEWAY_OUTBOUND_ENDPOINTS;
  credentials: Readonly<Record<GatewayCredentialName, string>>;
}

export interface LoadGatewayConfigOptions {
  env?: Readonly<Record<string, string | undefined>>;
  readCredential?: (directory: string, name: GatewayCredentialName) => string;
}

const MAX_CREDENTIAL_BYTES = 64 * 1024;
const MAX_PUBLIC_VALUE_BYTES = 1_024;
const SYMMETRIC_KEY_RE = /^[A-Za-z0-9_-]{43}$/;
const PUBLIC_SETTING_RE = /^[A-Za-z0-9._:-]+$/;

function flag(value: string | undefined, name: string): boolean {
  if (value === "0") return false;
  if (value === "1") return true;
  throw new Error(`${name} must be exactly 0 or 1`);
}

function publicSetting(value: string | undefined, name: string, required: boolean): string {
  const setting = value ?? "";
  if (setting.length === 0) {
    if (required) throw new Error(`${name} is required when its feature is enabled`);
    return setting;
  }
  if (
    Buffer.byteLength(setting, "utf8") > MAX_PUBLIC_VALUE_BYTES
    || !PUBLIC_SETTING_RE.test(setting)
  ) throw new Error(`${name} is invalid`);
  return setting;
}

function readCredentialFile(directory: string, name: GatewayCredentialName): string {
  const path = join(directory, name);
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.size <= 0 || stat.size > MAX_CREDENTIAL_BYTES) {
      throw new Error(`gateway credential ${name} is invalid`);
    }
    const value = readFileSync(fd, "utf8").trim();
    if (!value || Buffer.byteLength(value, "utf8") > MAX_CREDENTIAL_BYTES) {
      throw new Error(`gateway credential ${name} is invalid`);
    }
    return value;
  } finally {
    closeSync(fd);
  }
}

function validateCredentials(credentials: Record<GatewayCredentialName, string>): void {
  for (const name of ["google-client-secret", "turnstile-secret", "resend-api-key"] as const) {
    const value = credentials[name];
    if (
      value.length === 0
      || Buffer.byteLength(value, "utf8") > MAX_CREDENTIAL_BYTES
      || /[\u0000-\u001f\u007f]/u.test(value)
    ) throw new Error(`gateway credential ${name} is invalid`);
  }
  for (const name of ["email-outbox-key", "oauth-state-key"] as const) {
    const value = credentials[name];
    if (!SYMMETRIC_KEY_RE.test(value)) throw new Error(`gateway credential ${name} is invalid`);
    const decoded = Buffer.from(value, "base64url");
    if (decoded.byteLength !== 32 || decoded.toString("base64url") !== value) {
      throw new Error(`gateway credential ${name} is invalid`);
    }
  }
  try {
    const privateKey = createPrivateKey(credentials["assertion-signing-key"]);
    if (privateKey.asymmetricKeyType !== "ed25519") throw new Error("wrong key type");
  } catch {
    throw new Error("gateway credential assertion-signing-key is invalid");
  }
}

export function loadGatewayConfig(options: LoadGatewayConfigOptions = {}): GatewayConfig {
  const env = options.env ?? process.env;
  const signupEnabled = flag(env.ROOST_SIGNUP_ENABLED, "ROOST_SIGNUP_ENABLED");
  const googleEnabled = flag(env.ROOST_GOOGLE_ENABLED, "ROOST_GOOGLE_ENABLED");
  const googleClientId = publicSetting(
    env.ROOST_GOOGLE_OIDC_CLIENT_ID,
    "ROOST_GOOGLE_OIDC_CLIENT_ID",
    googleEnabled,
  );
  const turnstileSiteKey = publicSetting(
    env.ROOST_TURNSTILE_SITE_KEY,
    "ROOST_TURNSTILE_SITE_KEY",
    signupEnabled,
  );
  if (env.ROOST_SIGNUP_EMAIL_FROM !== SIGNUP_EMAIL_FROM) {
    throw new Error(`ROOST_SIGNUP_EMAIL_FROM must be exactly ${SIGNUP_EMAIL_FROM}`);
  }

  const credentialDirectory = env.CREDENTIALS_DIRECTORY;
  if (!credentialDirectory || !isAbsolute(credentialDirectory)) {
    throw new Error("CREDENTIALS_DIRECTORY must be an absolute path");
  }
  const readCredential = options.readCredential ?? readCredentialFile;
  const credentials = Object.fromEntries(
    GATEWAY_CREDENTIAL_NAMES.map((name) => [name, readCredential(credentialDirectory, name).trim()]),
  ) as Record<GatewayCredentialName, string>;
  validateCredentials(credentials);

  return Object.freeze({
    signupEnabled,
    googleEnabled,
    googleClientId,
    turnstileSiteKey,
    emailFrom: SIGNUP_EMAIL_FROM,
    publicOrigin: GATEWAY_PUBLIC_ORIGIN,
    listenHost: GATEWAY_LISTEN_HOST,
    listenPort: GATEWAY_LISTEN_PORT,
    stateDatabasePath: GATEWAY_STATE_DATABASE,
    outbound: GATEWAY_OUTBOUND_ENDPOINTS,
    credentials: Object.freeze(credentials),
  });
}
