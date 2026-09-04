import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { PRIVATE_IPC_SOCKET_PATH } from "../src/saas-auth/private-ipc.ts";
import { verifySystemdUnitSyntax } from "./systemd-unit-verification.ts";

const REPO_ROOT = resolve(import.meta.dir, "../../..");
const SYSTEMD_DIR = resolve(REPO_ROOT, "assets/linux/systemd");
const readAsset = (path: string): string => readFileSync(resolve(REPO_ROOT, path), "utf8");
const directiveValues = (text: string, name: string): string[] => text
  .split("\n")
  .filter((line) => line.startsWith(`${name}=`))
  .map((line) => line.slice(name.length + 1));

const authUnit = readAsset("assets/linux/systemd/roost-saas-auth.service");
const provisionerUnit = readAsset("assets/linux/systemd/roost-saas-provisioner.service");
const bridgeUnit = readAsset("assets/linux/systemd/roost-saas-auth-bridge.service");
const bridgeSocket = readAsset("assets/linux/systemd/roost-saas-auth-bridge.socket");
const isolationUnit = readAsset("assets/linux/systemd/roost-saas-origin-isolation.service");
const originFirewall = readAsset("assets/linux/nftables/roost-saas-origin-isolation.nft");
const resolverUnit = readAsset("assets/linux/systemd/roost-saas-resolver.service");
const resolverBridgeUnit = readAsset("assets/linux/systemd/roost-saas-resolver-bridge.service");
const resolverBridgeSocket = readAsset("assets/linux/systemd/roost-saas-resolver-bridge.socket");
const tunnelDropIn = readAsset("assets/linux/systemd/cloudflared.service.d/roost-saas.conf");
const tunnelConfig = readAsset("assets/linux/cloudflared/config.yml.example");
const systemUsers = readAsset("assets/linux/sysusers.d/roost-saas.conf");

const credentialMappings = [
  "google-client-secret:/etc/roost/saas-auth/google-client-secret",
  "turnstile-secret:/etc/roost/saas-auth/turnstile-secret",
  "resend-api-key:/etc/roost/saas-auth/resend-api-key",
  "email-outbox-key:/etc/roost/saas-auth/email-outbox-key",
  "oauth-state-key:/etc/roost/saas-auth/oauth-state-key",
  "assertion-signing-key:/etc/roost/saas-auth/assertion-signing-key",
];

const providerSecretNames = [
  "google-client-secret",
  "turnstile-secret",
  "resend-api-key",
  "email-outbox-key",
  "oauth-state-key",
  "assertion-signing-key",
];

describe("disabled-safe SaaS deployment assets", () => {
  test("ships public gates off and pins production provider metadata", () => {
    expect(directiveValues(authUnit, "Environment")).toContain("ROOST_SIGNUP_ENABLED=0");
    expect(directiveValues(authUnit, "Environment")).toContain("ROOST_GOOGLE_ENABLED=0");
    expect(authUnit).not.toContain("ROOST_SIGNUP_ENABLED=1");
    expect(authUnit).not.toContain("ROOST_GOOGLE_ENABLED=1");

    for (const expected of [
      'project "Roost Dashboard Production"',
      "homepage https://roosttt.com/",
      "privacy https://roosttt.com/privacy/",
      "terms https://roosttt.com/terms/",
      "support/developer contact support@roosttt.com",
      'Web client "Roost dashboard production"',
      "no JavaScript origins",
      "https://dashboard.roosttt.com/auth/google/callback",
      'scopes "openid email"',
      'Managed widget "Roost dashboard signup production"',
      "hostname dashboard.roosttt.com; action signup",
    ]) {
      expect(authUnit).toContain(expected);
    }
  });

  test("pins gateway credentials, public verification key, capacity, and private IPC", () => {
    expect(directiveValues(authUnit, "LoadCredential")).toEqual(credentialMappings);
    expect(authUnit).toContain("User=roost-signup\nGroup=roost-signup");
    expect(authUnit).toContain("StateDirectory=roost-signup");
    expect(authUnit).toContain("InaccessiblePaths=/run/docker.sock /srv/data/roost /srv/infra/edge");
    expect(authUnit).toContain("Requires=roost-saas-provisioner.service roost-saas-origin-isolation.service");

    expect(provisionerUnit).toContain("Environment=ROOST_SAAS_MAX_ACCOUNTS=8");
    expect(provisionerUnit).toContain("ConditionFileNotEmpty=/etc/roost/saas-auth/assertion-verify-key");
    expect(provisionerUnit).toContain("Environment=ROOST_SAAS_AUTH_VERIFY_KEY_FILE=/etc/roost/saas-auth/assertion-verify-key");
    expect(provisionerUnit).toContain("RuntimeDirectory=roost-saas-private");
    expect(provisionerUnit).toContain("RuntimeDirectoryMode=0750");
    expect(PRIVATE_IPC_SOCKET_PATH).toBe("/run/roost-saas-private/provision.sock");

    for (const secret of providerSecretNames) expect(provisionerUnit).not.toContain(secret);
    expect(provisionerUnit).not.toContain("ROOST_GOOGLE_OIDC_CLIENT_ID");
    expect(provisionerUnit).not.toContain("ROOST_TURNSTILE_SITE_KEY");
    expect(provisionerUnit).toContain("SocketBindDeny=ipv4:tcp");
    expect(provisionerUnit).toContain("SocketBindDeny=ipv6:tcp");
    expect(provisionerUnit).not.toContain("ListenStream=");
    expect(provisionerUnit).not.toMatch(/ExecStart=.*(?:--host|--port|--listen)/);
  });

  test("pins bridge, service identities, loopback ingress, and owner firewall rules", () => {
    expect(systemUsers).toContain('u roost-signup - "Roost signup gateway"');
    expect(systemUsers).toContain('u cloudflared - "Cloudflare Tunnel for Roost"');
    expect(tunnelDropIn).toContain("User=cloudflared\nGroup=cloudflared");
    expect(tunnelConfig).toContain("hostname: dashboard.roosttt.com\n    service: http://127.0.0.1:8080");

    expect(bridgeSocket).toContain("ListenStream=/run/roost-edge/auth.sock");
    expect(bridgeSocket).toContain("SocketUser=root\nSocketGroup=root\nSocketMode=0600");
    expect(bridgeUnit).toContain("User=root\nGroup=root");
    expect(bridgeUnit).toContain("ExecStart=/usr/lib/systemd/systemd-socket-proxyd 127.0.0.1:4108");
    expect(bridgeUnit).toContain("Requires=roost-saas-auth-bridge.socket roost-saas-auth.service roost-saas-origin-isolation.service");
    expect(resolverUnit).toContain("Requires=roost-saas-origin-isolation.service");
    expect(resolverUnit).toContain("ExecStart=/usr/local/bin/roost saas resolver");
    expect(resolverBridgeSocket).toContain("ListenStream=/run/roost-edge/resolver.sock");
    expect(resolverBridgeSocket).toContain("SocketMode=0600");
    expect(resolverBridgeUnit).toContain("User=root\nGroup=root");
    expect(resolverBridgeUnit).toContain("ExecStart=/usr/lib/systemd/systemd-socket-proxyd 127.0.0.1:4107");
    expect(provisionerUnit).toContain("Requires=docker.service roost-saas-origin-isolation.service roost-saas-resolver.service roost-saas-resolver-bridge.service");
    expect(authUnit).toContain("SocketBindAllow=tcp:4108\nSocketBindDeny=any");

    expect(isolationUnit).toContain("Before=cloudflared.service roost-saas-auth.service roost-saas-auth-bridge.service");
    expect(isolationUnit).toContain(
      "roost-saas-resolver.service roost-saas-resolver-bridge.service",
    );
    expect(originFirewall).toContain('oifname "lo" tcp dport 8080 meta skuid 0 accept');
    expect(originFirewall).toContain('oifname "lo" tcp dport 8080 meta skuid "cloudflared" accept');
    expect(originFirewall).toContain('oifname "lo" tcp dport 8080 reject with tcp reset');
    expect(originFirewall).toContain('oifname "lo" tcp dport 4108 meta skuid 0 accept');
    expect(originFirewall).toContain('oifname "lo" tcp dport 4108 reject with tcp reset');
    expect(originFirewall).toContain('oifname "lo" tcp dport 4107 meta skuid 0 accept');
    expect(originFirewall).toContain('oifname "lo" tcp dport 4107 reject with tcp reset');
    expect(originFirewall).not.toMatch(/dport 4107 meta skuid "(?:roost-signup|cloudflared)" accept/);
    expect(originFirewall).not.toMatch(/dport 4108 meta skuid "roost-signup" accept/);
  });

  test.skipIf(Bun.which("systemd-analyze") === null)("systemd accepts every auth isolation unit", () => {
    const units = [
      "roost-saas-auth.service",
      "roost-saas-provisioner.service",
      "roost-saas-auth-bridge.service",
      "roost-saas-auth-bridge.socket",
      "roost-saas-origin-isolation.service",
      "roost-saas-resolver.service",
      "roost-saas-resolver-bridge.service",
      "roost-saas-resolver-bridge.socket",
    ].map((name) => resolve(SYSTEMD_DIR, name));
    expect(verifySystemdUnitSyntax(Bun.which("systemd-analyze") as string, units)).toBe(0);
  });
});
