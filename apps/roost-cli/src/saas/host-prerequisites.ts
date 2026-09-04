// Verifies the privileged services and isolation boundaries required by SaaS.
// Host and provisioner startup call this sweep before accepting tenant work.
// Checks cover accounts, sockets, services, firewall policy, and direct-access denial.
import { existsSync, lstatSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { CommandRunner } from "./docker.ts";
import {
  AUTH_EDGE_SOCKET,
  CADDY_EDGE_SOCKET_DIR,
  ORIGIN_FIREWALL_TABLE,
  PRIVATE_PROVISION_DIR,
  PRIVATE_PROVISION_SOCKET,
  RESOLVER_EDGE_SOCKET,
  type SaasHostConfig,
} from "./host-config.ts";
import {
  assertDirectAccessRejected,
  assertDisk,
  assertFileMetadata,
  assertLoopbackListener,
  assertProperties,
  checkedCommand,
  checkedSecretFile,
  defaultRunner,
  diskUsedRatio,
  parseProperties,
  parseServiceAccount,
  parseServiceGroup,
} from "./host-prerequisite-checks.ts";

export function assertSaasProvisionerStartupPrerequisites(
  config: SaasHostConfig,
  runner: CommandRunner = defaultRunner,
  ratio: () => number = diskUsedRatio,
  onAlert: (message: string) => void = () => {},
): Promise<void> {
  return assertSaasHostPrerequisites(config, runner, ratio, onAlert, false);
}

export async function assertSaasHostPrerequisites(
  config: SaasHostConfig,
  runner: CommandRunner = defaultRunner,
  ratio: () => number = diskUsedRatio,
  onAlert: (message: string) => void = () => {},
  requireRuntimeServices = true,
): Promise<void> {
  assertDisk(ratio(), onAlert);
  checkedSecretFile(config.sharedResendApiKeyPath, "shared Resend API key file");
  checkedSecretFile(config.ageIdentityFile, "age identity file");
  if (!existsSync(config.caddyConfDir) || !lstatSync(config.caddyConfDir).isDirectory()) {
    throw new Error("Caddy conf.d prerequisite is missing");
  }
  const caddyfile = readFileSync(config.caddyfilePath, "utf8");
  if (!caddyfile.includes("admin 127.0.0.1:2019")
    || !caddyfile.includes("import /etc/caddy/conf.d/*.caddy")
    || !/respond\s+"not found"\s+404/.test(caddyfile)
    || !existsSync(join(config.caddyConfDir, "roost-tenants.caddy"))) {
    throw new Error("Caddy base configuration is not SaaS-ready");
  }
  const cloudflared = readFileSync(config.cloudflaredConfigPath, "utf8");
  const credentialMatch = /(?:^|\n)[ \t]*credentials-file:[ \t]*(?:"([^"]+)"|'([^']+)'|([^ \t\r\n]+))[ \t]*(?:\r?\n|$)/.exec(cloudflared);
  const cloudflaredCredentialPath = credentialMatch?.[1] ?? credentialMatch?.[2] ?? credentialMatch?.[3];
  if (!cloudflaredCredentialPath
    || !/^\/etc\/cloudflared\/[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}[.]json$/.test(cloudflaredCredentialPath)) {
    throw new Error("Cloudflare tunnel credentials path is not SaaS-ready");
  }
  const dashboardRoute = /(?:^|\n)[ \t]*-[ \t]*hostname:[ \t]*(?:"dashboard[.]roosttt[.]com"|'dashboard[.]roosttt[.]com'|dashboard[.]roosttt[.]com)[ \t]*\r?\n[ \t]+service:[ \t]*http:\/\/127[.]0[.]0[.]1:8080[ \t]*(?:\r?\n|$)/.exec(cloudflared);
  const terminal404 = cloudflared.lastIndexOf("service: http_status:404");
  if (!dashboardRoute || terminal404 <= dashboardRoute.index + dashboardRoute[0].length) {
    throw new Error("Cloudflare dashboard tunnel route is not configured in default-deny order");
  }
  const roostSignup = parseServiceAccount(
    await checkedCommand(runner, "roost-signup user", ["getent", "passwd", "roost-signup"]),
    "roost-signup",
  );
  const cloudflaredUser = parseServiceAccount(
    await checkedCommand(runner, "cloudflared user", ["getent", "passwd", "cloudflared"]),
    "cloudflared",
  );
  if (roostSignup.uid === cloudflaredUser.uid || roostSignup.gid === cloudflaredUser.gid) {
    throw new Error("origin services must use isolated users and groups");
  }
  parseServiceGroup(
    await checkedCommand(runner, "roost-signup group", ["getent", "group", "roost-signup"]),
    "roost-signup",
    roostSignup.gid,
  );
  parseServiceGroup(
    await checkedCommand(runner, "cloudflared group", ["getent", "group", "cloudflared"]),
    "cloudflared",
    cloudflaredUser.gid,
  );
  await checkedCommand(runner, "Caddy validation", [
    "docker", "exec", "caddy", "caddy", "validate", "--config", "/etc/caddy/Caddyfile",
  ]);
  await checkedCommand(runner, "Cloudflare ingress validation", [
    "runuser", "--user", "cloudflared", "--",
    "cloudflared", "--config", config.cloudflaredConfigPath, "tunnel", "ingress", "validate",
  ]);
  assertFileMetadata(
    await checkedCommand(runner, "Cloudflare config metadata", [
      "stat", "--format=%F|%a|%U|%G", "--", config.cloudflaredConfigPath,
    ]),
    "regular file|640|root|cloudflared",
    "Cloudflare config",
  );
  assertFileMetadata(
    await checkedCommand(runner, "Cloudflare credential metadata", [
      "stat", "--format=%F|%a|%U|%G", "--", cloudflaredCredentialPath,
    ]),
    "regular file|640|root|cloudflared",
    "Cloudflare credentials",
  );
  assertFileMetadata(
    await checkedCommand(runner, "Cloudflare directory metadata", [
      "stat", "--format=%F|%a|%U|%G", "--", "/etc/cloudflared",
    ]),
    "directory|750|root|cloudflared",
    "Cloudflare directory",
  );
  const serviceProperties = [
    "--property=LoadState",
    "--property=ActiveState",
    "--property=User",
    "--property=Group",
    "--property=ExecStart",
    "--no-pager",
  ] as const;
  if (requireRuntimeServices) {
    assertProperties(
      await checkedCommand(runner, "signup gateway service", [
        "systemctl", "show", "roost-saas-auth.service", ...serviceProperties,
      ]),
      "signup gateway service",
      { LoadState: "loaded", ActiveState: "active", User: "roost-signup", Group: "roost-signup" },
      "/usr/local/bin/roost __saas-auth serve",
    );
    assertProperties(
      await checkedCommand(runner, "SaaS provisioner service", [
        "systemctl", "show", "roost-saas-provisioner.service", ...serviceProperties,
      ]),
      "SaaS provisioner service",
      { LoadState: "loaded", ActiveState: "active", User: "root", Group: "roost-signup" },
      "/usr/local/bin/roost __saas-provisioner serve",
    );
    assertProperties(
      await checkedCommand(runner, "Cloudflare service", [
        "systemctl", "show", "cloudflared.service", ...serviceProperties,
      ]),
      "Cloudflare service",
      { LoadState: "loaded", ActiveState: "active", User: "cloudflared", Group: "cloudflared" },
      "/usr/bin/cloudflared --no-autoupdate --config /etc/cloudflared/config.yml tunnel run",
    );
    assertProperties(
      await checkedCommand(runner, "auth bridge service", [
        "systemctl", "show", "roost-saas-auth-bridge.service", ...serviceProperties,
      ]),
      "auth bridge service",
      { LoadState: "loaded", ActiveState: "active", User: "root", Group: "root" },
      "/usr/lib/systemd/systemd-socket-proxyd 127.0.0.1:4108",
    );
    const authSocketProperties = parseProperties(
      await checkedCommand(runner, "auth bridge socket", [
        "systemctl", "show", "roost-saas-auth-bridge.socket",
        "--property=LoadState", "--property=ActiveState", "--property=Listen",
        "--property=Accept", "--property=SocketMode", "--property=DirectoryMode", "--no-pager",
      ]),
      "auth bridge socket",
    );
    for (const [name, value] of Object.entries({
      LoadState: "loaded",
      ActiveState: "active",
      Listen: `${AUTH_EDGE_SOCKET} (Stream)`,
      Accept: "no",
      SocketMode: "0600",
      DirectoryMode: "0755",
    })) {
      if (authSocketProperties.get(name) !== value) {
        throw new Error(`auth bridge socket has unsafe ${name}`);
      }
    }
    assertFileMetadata(
      await checkedCommand(runner, "auth bridge socket metadata", [
        "stat", "--format=%F|%a|%U|%G", "--", AUTH_EDGE_SOCKET,
      ]),
      "socket|600|root|root",
      "auth bridge socket",
    );
    assertFileMetadata(
      await checkedCommand(runner, "private provision socket metadata", [
        "stat", "--format=%F|%a|%U|%G", "--", PRIVATE_PROVISION_SOCKET,
      ]),
      "socket|660|root|roost-signup",
      "private provision socket",
    );
  }
  assertFileMetadata(
    await checkedCommand(runner, "private provision directory metadata", [
      "stat", "--format=%F|%a|%U|%G", "--", PRIVATE_PROVISION_DIR,
    ]),
    "directory|750|root|roost-signup",
    "private provision directory",
  );
  assertProperties(
    await checkedCommand(runner, "tenant resolver service", [
      "systemctl", "show", "roost-saas-resolver.service", ...serviceProperties,
    ]),
    "tenant resolver service",
    { LoadState: "loaded", ActiveState: "active", User: "root", Group: "root" },
    "/usr/local/bin/roost saas resolver",
  );
  assertProperties(
    await checkedCommand(runner, "resolver bridge service", [
      "systemctl", "show", "roost-saas-resolver-bridge.service", ...serviceProperties,
    ]),
    "resolver bridge service",
    { LoadState: "loaded", ActiveState: "active", User: "root", Group: "root" },
    "/usr/lib/systemd/systemd-socket-proxyd 127.0.0.1:4107",
  );
  const resolverSocketProperties = parseProperties(
    await checkedCommand(runner, "resolver bridge socket", [
      "systemctl", "show", "roost-saas-resolver-bridge.socket",
      "--property=LoadState", "--property=ActiveState", "--property=Listen",
      "--property=Accept", "--property=SocketMode", "--property=DirectoryMode", "--no-pager",
    ]),
    "resolver bridge socket",
  );
  for (const [name, value] of Object.entries({
    LoadState: "loaded",
    ActiveState: "active",
    Listen: `${RESOLVER_EDGE_SOCKET} (Stream)`,
    Accept: "no",
    SocketMode: "0600",
    DirectoryMode: "0755",
  })) {
    if (resolverSocketProperties.get(name) !== value) {
      throw new Error(`resolver bridge socket has unsafe ${name}`);
    }
  }
  assertFileMetadata(
    await checkedCommand(runner, "resolver bridge socket metadata", [
      "stat", "--format=%F|%a|%U|%G", "--", RESOLVER_EDGE_SOCKET,
    ]),
    "socket|600|root|root",
    "resolver bridge socket",
  );
  const isolationService = await checkedCommand(runner, "origin isolation service", [
    "systemctl", "show", "roost-saas-origin-isolation.service",
    "--property=LoadState", "--property=ActiveState", "--no-pager",
  ]);
  assertProperties(
    isolationService,
    "origin isolation service",
    { LoadState: "loaded", ActiveState: "active" },
  );
  if (await checkedCommand(runner, "persistent origin isolation", [
    "systemctl", "is-enabled", "roost-saas-origin-isolation.service",
  ]) !== "enabled") {
    throw new Error("origin isolation firewall is not persistently enabled");
  }
  assertLoopbackListener(
    await checkedCommand(runner, "Caddy listener", [
      "ss", "--no-header", "--listening", "--tcp", "--numeric", "sport = :8080",
    ]),
    8080,
    "Caddy listener",
  );
  if (requireRuntimeServices) {
    assertLoopbackListener(
      await checkedCommand(runner, "signup gateway listener", [
        "ss", "--no-header", "--listening", "--tcp", "--numeric", "sport = :4108",
      ]),
      4108,
      "signup gateway listener",
    );
  }
  assertLoopbackListener(
    await checkedCommand(runner, "tenant resolver listener", [
      "ss", "--no-header", "--listening", "--tcp", "--numeric", "sport = :4107",
    ]),
    4107,
    "tenant resolver listener",
  );
  const firewall = (await checkedCommand(runner, "origin isolation firewall", [
    "nft", "-nn", "list", "chain", "inet", ORIGIN_FIREWALL_TABLE, "output",
  ])).replace(/\s+/g, " ");
  const requiredRules = [
    'oifname "lo" tcp dport 8080 meta skuid 0 accept',
    `oifname "lo" tcp dport 8080 meta skuid ${cloudflaredUser.uid} accept`,
    'oifname "lo" tcp dport 8080 reject with tcp reset',
    'oifname "lo" tcp dport 4108 meta skuid 0 accept',
    'oifname "lo" tcp dport 4108 reject with tcp reset',
    'oifname "lo" tcp dport 4107 meta skuid 0 accept',
    'oifname "lo" tcp dport 4107 reject with tcp reset',
  ];
  let firewallOffset = 0;
  for (const rule of requiredRules) {
    const next = firewall.indexOf(rule, firewallOffset);
    if (next < 0) throw new Error("origin isolation firewall rules are missing or out of order");
    firewallOffset = next + rule.length;
  }
  await assertDirectAccessRejected(
    runner,
    "Caddy listener",
    "roost-signup",
    "http://127.0.0.1:8080/",
  );
  if (requireRuntimeServices) {
    await assertDirectAccessRejected(
      runner,
      "signup gateway listener",
      "cloudflared",
      "http://127.0.0.1:4108/__roost/auth/config",
    );
  }
  await assertDirectAccessRejected(
    runner,
    "tenant resolver listener",
    "roost-signup",
    "http://127.0.0.1:4107/healthz",
  );
  const image = await checkedCommand(runner, "coordinator image", [
    "docker", "image", "inspect", config.imageDigest, "--format", "{{.Id}}",
  ]);
  if (image !== config.imageDigest) throw new Error("coordinator image ID does not match configured immutable digest");
  await checkedCommand(runner, "Docker network", ["docker", "network", "inspect", config.network]);
  const caddyImage = await checkedCommand(runner, "Caddy image", [
    "docker", "inspect", "caddy", "--format", "{{.Config.Image}}",
  ]);
  if (caddyImage !== config.caddyImageDigest) throw new Error("Caddy is not running the pinned image digest");
  const caddyMountsRaw = await checkedCommand(runner, "Caddy edge socket mount", [
    "docker", "inspect", "caddy", "--format", "{{json .Mounts}}",
  ]);
  let caddyMounts: unknown;
  try {
    caddyMounts = JSON.parse(caddyMountsRaw);
  } catch {
    throw new Error("Caddy edge socket mount prerequisite returned invalid JSON");
  }
  if (!Array.isArray(caddyMounts)
    || !caddyMounts.some((entry) => {
      if (!entry || typeof entry !== "object") return false;
      const mount = entry as Record<string, unknown>;
      return mount.Type === "bind"
        && mount.Source === CADDY_EDGE_SOCKET_DIR
        && mount.Destination === CADDY_EDGE_SOCKET_DIR
        && mount.RW === false;
    })) {
    throw new Error("Caddy is missing the read-only edge socket mount");
  }
  if (caddyMounts.some((entry) => {
    if (!entry || typeof entry !== "object") return false;
    const mount = entry as Record<string, unknown>;
    return [mount.Source, mount.Destination].some((path) => {
      if (typeof path !== "string") return false;
      const normalized = path.replace(/\/+$/, "") || "/";
      return normalized === PRIVATE_PROVISION_DIR
        || normalized.startsWith(`${PRIVATE_PROVISION_DIR}/`)
        || PRIVATE_PROVISION_DIR.startsWith(`${normalized}/`);
    });
  })) {
    throw new Error("Caddy must not mount the private provisioner socket");
  }
  await checkedCommand(runner, "Caddy private socket isolation", [
    "docker", "exec", "caddy", "test", "!", "-e", PRIVATE_PROVISION_SOCKET,
  ]);
  await checkedCommand(runner, "age", ["age", "--version"]);
}
