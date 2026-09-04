import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import {
  HostAdmission,
  assertSaasHostPrerequisites,
  assertSaasProvisionerStartupPrerequisites,
  loadSaasHostConfig,
} from "../src/saas/host.ts";
import { SaasRegistry } from "../src/saas/registry.ts";
import type { CommandRunner } from "../src/saas/docker.ts";
import { writeEd25519VerificationKeyFixture } from "./managed-e2e-fixture.ts";

const IMAGE = `sha256:${"a".repeat(64)}`;
const ACCOUNT_ID = "11111111-1111-4111-8111-111111111111";
const COORDINATOR_ID = "22222222-2222-4222-8222-222222222222";
const cleanups: string[] = [];
afterEach(() => {
  while (cleanups.length > 0) rmSync(cleanups.pop()!, { recursive: true, force: true });
});

function hostFixture() {
  const root = mkdtempSync(join(tmpdir(), "roost-saas-host-"));
  cleanups.push(root);
  const confDir = join(root, "conf.d");
  mkdirSync(confDir, { mode: 0o700 });
  writeFileSync(join(confDir, "roost-tenants.caddy"), "");
  const resend = join(root, "resend-key");
  const ageIdentity = join(root, "age-key");
  const authVerifyKey = join(root, "saas-auth-verify-key");
  writeFileSync(resend, "re_test", { mode: 0o600 });
  writeFileSync(ageIdentity, "AGE-SECRET-KEY-TEST", { mode: 0o600 });
  writeEd25519VerificationKeyFixture(authVerifyKey);
  const caddyfile = join(root, "Caddyfile");
  writeFileSync(
    caddyfile,
    "{\n admin 127.0.0.1:2019\n}\nimport /etc/caddy/conf.d/*.caddy\n:80 {\n respond \"not found\" 404\n}\n",
  );
  const cloudflared = join(root, "cloudflared.yml");
  writeFileSync(
    cloudflared,
    "tunnel: aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa\n"
      + "credentials-file: /etc/cloudflared/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.json\n"
      + "ingress:\n"
      + "  - hostname: dashboard.roosttt.com\n  service: http://127.0.0.1:8080\n"
      + "  - service: http_status:404\n",
  );
  const config = loadSaasHostConfig({
    ROOST_SAAS_ROOT: root,
    ROOST_SAAS_IMAGE_DIGEST: IMAGE,
    ROOST_SAAS_MAX_ACCOUNTS: "8",
    ROOST_SAAS_RESEND_ENDPOINT: "https://api.resend.com/emails",
    ROOST_SAAS_EMAIL_FROM: "noreply@example.com",
    ROOST_SAAS_RESEND_API_KEY_FILE: resend,
    ROOST_SAAS_AUTH_VERIFY_KEY_FILE: authVerifyKey,
    ROOST_SAAS_AGE_RECIPIENT: `age1${"a".repeat(58)}`,
    ROOST_SAAS_AGE_IDENTITY_FILE: ageIdentity,
    ROOST_SAAS_CADDY_CONF_DIR: confDir,
    ROOST_SAAS_CADDYFILE: caddyfile,
    ROOST_SAAS_CLOUDFLARED_CONFIG: cloudflared,
  });
  return { root, config };
}

const runner: CommandRunner = async (argv) => {
  if (argv[0] === "getent" && argv[1] === "passwd") {
    const name = argv[2]!;
    const id = name === "roost-signup" ? 991 : 992;
    return {
      exitCode: 0,
      stdout: `${name}:x:${id}:${id}:${name}:/var/lib/${name}:/usr/sbin/nologin\n`,
      stderr: "",
    };
  }
  if (argv[0] === "getent" && argv[1] === "group") {
    const name = argv[2]!;
    const id = name === "roost-signup" ? 991 : 992;
    return { exitCode: 0, stdout: `${name}:x:${id}:\n`, stderr: "" };
  }
  if (argv[0] === "stat") {
    const path = argv.at(-1)!;
    const metadata = path === "/etc/cloudflared"
      ? "directory|750|root|cloudflared"
      : path === "/run/roost-edge/auth.sock" || path === "/run/roost-edge/resolver.sock"
        ? "socket|600|root|root"
        : path === "/run/roost-saas-private"
          ? "directory|750|root|roost-signup"
          : path === "/run/roost-saas-private/provision.sock"
            ? "socket|660|root|roost-signup"
            : "regular file|640|root|cloudflared";
    return { exitCode: 0, stdout: `${metadata}\n`, stderr: "" };
  }
  if (argv[0] === "systemctl" && argv[1] === "is-enabled") {
    return { exitCode: 0, stdout: "enabled\n", stderr: "" };
  }
  if (argv[0] === "systemctl" && argv[1] === "show") {
    const services: Record<string, string> = {
      "roost-saas-auth.service":
        "LoadState=loaded\nActiveState=active\nUser=roost-signup\nGroup=roost-signup\nExecStart=/usr/local/bin/roost __saas-auth serve\n",
      "roost-saas-provisioner.service":
        "LoadState=loaded\nActiveState=active\nUser=root\nGroup=roost-signup\nExecStart=/usr/local/bin/roost __saas-provisioner serve\n",
      "cloudflared.service":
        "LoadState=loaded\nActiveState=active\nUser=cloudflared\nGroup=cloudflared\nExecStart=/usr/bin/cloudflared --no-autoupdate --config /etc/cloudflared/config.yml tunnel run\n",
      "roost-saas-auth-bridge.service":
        "LoadState=loaded\nActiveState=active\nUser=root\nGroup=root\nExecStart=/usr/lib/systemd/systemd-socket-proxyd 127.0.0.1:4108\n",
      "roost-saas-auth-bridge.socket":
        "LoadState=loaded\nActiveState=active\nListen=/run/roost-edge/auth.sock (Stream)\nAccept=no\nSocketMode=0600\nDirectoryMode=0755\n",
      "roost-saas-resolver.service":
        "LoadState=loaded\nActiveState=active\nUser=root\nGroup=root\nExecStart=/usr/local/bin/roost saas resolver\n",
      "roost-saas-resolver-bridge.service":
        "LoadState=loaded\nActiveState=active\nUser=root\nGroup=root\nExecStart=/usr/lib/systemd/systemd-socket-proxyd 127.0.0.1:4107\n",
      "roost-saas-resolver-bridge.socket":
        "LoadState=loaded\nActiveState=active\nListen=/run/roost-edge/resolver.sock (Stream)\nAccept=no\nSocketMode=0600\nDirectoryMode=0755\n",
    };
    return { exitCode: 0, stdout: services[argv[2] ?? ""] ?? "LoadState=loaded\nActiveState=active\n", stderr: "" };
  }
  if (argv[0] === "ss") {
    const filter = argv.at(-1) ?? "";
    const port = filter.includes("8080") ? 8080 : filter.includes("4107") ? 4107 : 4108;
    return {
      exitCode: 0,
      stdout: `LISTEN 0 4096 127.0.0.1:${port} 0.0.0.0:*\n`,
      stderr: "",
    };
  }
  if (argv[0] === "nft") {
    return {
      exitCode: 0,
      stdout: 'table inet roost_saas_origin { chain output { '
        + 'oifname "lo" tcp dport 8080 meta skuid 0 accept '
        + 'oifname "lo" tcp dport 8080 meta skuid 992 accept '
        + 'oifname "lo" tcp dport 8080 reject with tcp reset '
        + 'oifname "lo" tcp dport 4108 meta skuid 0 accept '
        + 'oifname "lo" tcp dport 4108 reject with tcp reset '
        + 'oifname "lo" tcp dport 4107 meta skuid 0 accept '
        + 'oifname "lo" tcp dport 4107 reject with tcp reset } }\n',
      stderr: "",
    };
  }
  if (argv[0] === "runuser" && argv.includes("curl")) {
    return { exitCode: 7, stdout: "", stderr: "connection rejected\n" };
  }
  if (argv[1] === "image") return { exitCode: 0, stdout: `${IMAGE}\n`, stderr: "" };
  if (argv[1] === "inspect" && argv[2] === "caddy") {
    if (argv.at(-1) === "{{json .Mounts}}") {
      return {
        exitCode: 0,
        stdout: '[{"Type":"bind","Source":"/run/roost-edge","Destination":"/run/roost-edge","RW":false}]\n',
        stderr: "",
      };
    }
    return {
      exitCode: 0,
      stdout: "caddy@sha256:5f5c8640aae01df9654968d946d8f1a56c497f1dd5c5cda4cf95ab7c14d58648\n",
      stderr: "",
    };
  }
  return { exitCode: 0, stdout: "ok\n", stderr: "" };
};

describe("SaaS host admission", () => {
  test("accepts exact immutable prerequisites, alerts at 70%, and stops at 85%", async () => {
    const opened = hostFixture();
    expect(opened.config.authVerifyKeyFile).toBe(join(opened.root, "saas-auth-verify-key"));
    const alerts: string[] = [];
    await expect(assertSaasHostPrerequisites(opened.config, runner, () => 0.84, (message) => alerts.push(message)))
      .resolves.toBeUndefined();
    expect(alerts).toEqual(["host disk usage is 84%"]);
    await expect(assertSaasHostPrerequisites(opened.config, runner, () => 0.85))
      .rejects.toThrow("at or above 85%");
  });

  test("rejects a legacy direct dashboard tunnel or missing Caddy edge socket mount", async () => {
    const opened = hostFixture();
    writeFileSync(
      opened.config.cloudflaredConfigPath,
      "credentials-file: /etc/cloudflared/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.json\n"
        + "ingress:\n"
        + "  - hostname: dashboard.roosttt.com\n  service: http://127.0.0.1:4104\n"
        + "  - service: http_status:404\n",
    );
    await expect(assertSaasHostPrerequisites(opened.config, runner, () => 0.5))
      .rejects.toThrow("dashboard tunnel route");

    writeFileSync(
      opened.config.cloudflaredConfigPath,
      "credentials-file: /etc/cloudflared/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.json\n"
        + "ingress:\n"
        + "  - hostname: dashboard.roosttt.com\n  service: http://127.0.0.1:8080\n"
        + "  - service: http_status:404\n",
    );
    const missingGateway: CommandRunner = async (argv) => {
      if (argv.at(-1) === "{{json .Mounts}}") {
        return { exitCode: 0, stdout: "[]\n", stderr: "" };
      }
      return runner(argv);
    };
    await expect(assertSaasHostPrerequisites(opened.config, missingGateway, () => 0.5))
      .rejects.toThrow("edge socket mount");
  });
  test("rejects unsafe origin users, modes, services, listeners, firewall, and socket exposure", async () => {
    const cases: Array<[string, CommandRunner]> = [
      ["dedicated non-login service user", async (argv) => {
        if (argv[0] === "getent" && argv[1] === "passwd" && argv[2] === "cloudflared") {
          return { exitCode: 0, stdout: "cloudflared:x:0:0:cloudflared:/root:/bin/bash\n", stderr: "" };
        }
        return runner(argv);
      }],
      ["unsafe ownership or mode", async (argv) => {
        if (argv[0] === "stat" && argv.at(-1)?.endsWith(".json")) {
          return { exitCode: 0, stdout: "regular file|600|root|root\n", stderr: "" };
        }
        return runner(argv);
      }],
      ["unsafe User", async (argv) => {
        if (argv[0] === "systemctl" && argv[2] === "cloudflared.service") {
          return {
            exitCode: 0,
            stdout: "LoadState=loaded\nActiveState=active\nUser=root\nGroup=root\n"
              + "ExecStart=/usr/bin/cloudflared --no-autoupdate --config /etc/cloudflared/config.yml tunnel run\n",
            stderr: "",
          };
        }
        return runner(argv);
      }],
      ["auth bridge socket has unsafe SocketMode", async (argv) => {
        if (argv[0] === "systemctl" && argv[2] === "roost-saas-auth-bridge.socket") {
          return {
            exitCode: 0,
            stdout: "LoadState=loaded\nActiveState=active\nListen=/run/roost-edge/auth.sock (Stream)\n"
              + "Accept=no\nSocketMode=0666\nDirectoryMode=0755\n",
            stderr: "",
          };
        }
        return runner(argv);
      }],
      ["must bind exactly 127.0.0.1:4108", async (argv) => {
        if (argv[0] === "ss" && argv.at(-1)?.includes("4108")) {
          return { exitCode: 0, stdout: "LISTEN 0 4096 0.0.0.0:4108 0.0.0.0:*\n", stderr: "" };
        }
        return runner(argv);
      }],
      ["firewall rules are missing or out of order", async (argv) => {
        if (argv[0] === "nft") return { exitCode: 0, stdout: "table inet roost_saas_origin {}\n", stderr: "" };
        return runner(argv);
      }],
      ["directly reachable by roost-signup", async (argv) => {
        if (argv[0] === "runuser" && argv.includes("curl")) {
          return { exitCode: 0, stdout: "", stderr: "" };
        }
        return runner(argv);
      }],
      ["must not mount the private provisioner socket", async (argv) => {
        if (argv.at(-1) === "{{json .Mounts}}") {
          return {
            exitCode: 0,
            stdout: '[{"Type":"bind","Source":"/run/roost-edge","Destination":"/run/roost-edge","RW":false},'
              + '{"Type":"bind","Source":"/run","Destination":"/run","RW":false}]\n',
            stderr: "",
          };
        }
        return runner(argv);
      }],
    ];
    for (const [message, unsafeRunner] of cases) {
      const opened = hostFixture();
      await expect(assertSaasHostPrerequisites(opened.config, unsafeRunner, () => 0.5))
        .rejects.toThrow(message);
    }
  });

  test("rejects tags before any registry or Docker mutation", () => {
    const opened = hostFixture();
    expect(() => loadSaasHostConfig({
      ROOST_SAAS_ROOT: opened.root,
      ROOST_SAAS_IMAGE_DIGEST: "roost-coord:latest",
      ROOST_SAAS_MAX_ACCOUNTS: "8",
      ROOST_SAAS_RESEND_ENDPOINT: "https://api.resend.com/emails",
      ROOST_SAAS_EMAIL_FROM: "noreply@example.com",
      ROOST_SAAS_RESEND_API_KEY_FILE: opened.config.sharedResendApiKeyPath,
      ROOST_SAAS_AGE_RECIPIENT: opened.config.ageRecipient,
      ROOST_SAAS_AGE_IDENTITY_FILE: opened.config.ageIdentityFile,
      ROOST_SAAS_AUTH_VERIFY_KEY_FILE: opened.config.authVerifyKeyFile,
    })).toThrow("immutable sha256 digest");
  });


  test("requires an absolute managed auth verify-key file", () => {
    const opened = hostFixture();
    const base = {
      ROOST_SAAS_ROOT: opened.root,
      ROOST_SAAS_IMAGE_DIGEST: IMAGE,
      ROOST_SAAS_MAX_ACCOUNTS: "8",
      ROOST_SAAS_RESEND_ENDPOINT: "https://api.resend.com/emails",
      ROOST_SAAS_EMAIL_FROM: "noreply@example.com",
      ROOST_SAAS_RESEND_API_KEY_FILE: opened.config.sharedResendApiKeyPath,
      ROOST_SAAS_AGE_RECIPIENT: opened.config.ageRecipient,
      ROOST_SAAS_AGE_IDENTITY_FILE: opened.config.ageIdentityFile,
    };
    expect(() => loadSaasHostConfig(base)).toThrow("ROOST_SAAS_AUTH_VERIFY_KEY_FILE is required");
    expect(() => loadSaasHostConfig({
      ...base,
      ROOST_SAAS_AUTH_VERIFY_KEY_FILE: "relative/verify-key",
    })).toThrow("must be an absolute path");
  });
  test("blocks new work when an active coordinator backup is older than 26 hours", async () => {
    const opened = hostFixture();
    const ids = [ACCOUNT_ID, COORDINATOR_ID];
    const registry = new SaasRegistry({
      rootDir: opened.root,
      path: join(opened.root, "control.db"),
      now: () => 1_000,
      createId: () => ids.shift()!,
    });
    try {
      registry.reserveAccount("owner@example.com", IMAGE);
      registry.markAccountActive(ACCOUNT_ID);
      registry.transitionCoordinator(COORDINATOR_ID, "reserved", "active");
      const admission = new HostAdmission({
        registry,
        config: opened.config,
        now: () => 30 * 60 * 60 * 1000,
        diskRatio: () => 0.5,
      });
      await expect(admission.assertBeforeReservation(() => undefined))
        .rejects.toThrow("no backup newer than 26 hours");
      const backupDir = join(opened.root, "backups", COORDINATOR_ID);
      mkdirSync(backupDir, { recursive: true });
      writeFileSync(join(backupDir, "recent.tar.age"), "encrypted");
      const freshAdmission = new HostAdmission({
        registry,
        config: opened.config,
        now: Date.now,
        diskRatio: () => 0.5,
      });
      await expect(freshAdmission.assertBeforeReservation(() => undefined)).resolves.toBeUndefined();
    } finally {
      registry.close();
    }
  });
});

describe("SaaS provisioner startup admission", () => {
  test("proves every non-circular runtime prerequisite before accepting jobs", async () => {
    const opened = hostFixture();
    const calls: readonly string[][] = [];
    const recordingRunner: CommandRunner = async (argv) => {
      (calls as string[][]).push([...argv]);
      return runner(argv);
    };
    await expect(assertSaasProvisionerStartupPrerequisites(
      opened.config,
      recordingRunner,
      () => 0.5,
    )).resolves.toBeUndefined();
    const commands = calls.map((argv) => argv.join(" "));
    expect(commands.some((command) => command.startsWith("nft -nn list chain inet roost_saas_origin output")))
      .toBe(true);
    expect(commands.some((command) => command.includes("sport = :4107"))).toBe(true);
    expect(commands.some((command) => command === `docker image inspect ${IMAGE} --format {{.Id}}`)).toBe(true);
    expect(commands.some((command) => command === "docker network inspect web")).toBe(true);
    expect(commands.some((command) => command === "age --version")).toBe(true);
    expect(commands.some((command) => command.includes("roost-saas-auth.service"))).toBe(false);

    const wrongImage: CommandRunner = async (argv) => {
      if (argv[0] === "docker" && argv[1] === "image") {
        return { exitCode: 0, stdout: `sha256:${"b".repeat(64)}\n`, stderr: "" };
      }
      return runner(argv);
    };
    await expect(assertSaasProvisionerStartupPrerequisites(opened.config, wrongImage, () => 0.5))
      .rejects.toThrow("image ID");
  });
});
