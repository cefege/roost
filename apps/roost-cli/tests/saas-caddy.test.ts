import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import {
  CaddyTenantRouter,
  renderCaddyTenantRoutes,
} from "../src/saas/caddy.ts";
import type { CommandResult } from "../src/saas/docker.ts";
import {
  coordinatorContainerName,
  type CoordinatorState,
  type RegistryCoordinator,
} from "../src/saas/registry.ts";

const FIRST_ID = "22222222-2222-4222-8222-222222222222";
const SECOND_ID = "33333333-3333-4333-8333-333333333333";
const cleanups: string[] = [];

function tempConfDir(): string {
  const path = mkdtempSync(join(tmpdir(), "roost-saas-caddy-"));
  cleanups.push(path);
  return path;
}

function routeKey(id: string): string {
  return id.replaceAll("-", "").repeat(2);
}

function coordinator(id: string, state: CoordinatorState): RegistryCoordinator {
  return {
    id,
    accountId: id,
    routeKey: routeKey(id),
    ordinal: 1,
    hostname: `c-${id.replaceAll("-", "")}.dashboard.roosttt.com`,
    containerName: coordinatorContainerName(id),
    dataDir: `/srv/data/roost/instances/${id}/data`,
    imageDigest: `sha256:${"a".repeat(64)}`,
    state,
    createdAtMs: 1,
    seededAtMs: null,
    runningAtMs: null,
    routedAtMs: null,
    invitedAtMs: null,
    activatedAtMs: null,
    disabledAtMs: null,
    failedAtMs: null,
    updatedAtMs: 1,
    lastError: null,
  };
}

function ok(): CommandResult {
  return { exitCode: 0, stdout: "", stderr: "" };
}

function finalPath(confDir: string): string {
  return join(confDir, "roost-tenants.caddy");
}


afterEach(() => {
  while (cleanups.length > 0) rmSync(cleanups.pop()!, { recursive: true, force: true });
});

describe("Caddy tenant routes", () => {
  test("renders two account routes in one deterministic shared site with resolver and legacy fallbacks", () => {
    const disabledId = "55555555-5555-4555-8555-555555555555";
    const reservedId = "66666666-6666-4666-8666-666666666666";
    const rows = [
      coordinator(SECOND_ID, "active"),
      coordinator(FIRST_ID, "invited"),
      coordinator("44444444-4444-4444-8444-444444444444", "routed"),
      coordinator(disabledId, "disabled"),
      coordinator(reservedId, "reserved"),
      coordinator("77777777-7777-4777-8777-777777777777", "seeded"),
      coordinator("88888888-8888-4888-8888-888888888888", "running"),
      coordinator("99999999-9999-4999-8999-999999999999", "failed"),
    ];

    const rendered = renderCaddyTenantRoutes(rows);
    expect(renderCaddyTenantRoutes([...rows].reverse())).toBe(rendered);
    expect(rendered.match(/http:\/\/dashboard[.]roosttt[.]com:8080/g)).toHaveLength(1);
    expect(rendered).not.toContain(".dashboard.roosttt.com:8080 {");
    const authEndpoints = [
      ["roost_auth_config", "GET", "/__roost/auth/config"],
      ["roost_signup_email_start", "POST", "/__roost/signup/email/start"],
      ["roost_signup_email_verify", "POST", "/__roost/signup/email/verify"],
      ["roost_auth_google_start", "POST", "/__roost/auth/google/start"],
      ["roost_auth_google_callback", "GET", "/auth/google/callback"],
      ["roost_auth_result", "GET", "/__roost/auth/result"],
      ["roost_auth_bind_device", "POST", "/__roost/auth/bind-device"],
      ["roost_auth_link_complete", "POST", "/__roost/auth/link/complete"],
    ] as const;
    for (const [matcher, method, path] of authEndpoints) {
      expect(rendered).toContain(
        `\t@${matcher} {\n`
        + `\t\tmethod ${method}\n`
        + `\t\tpath ${path}\n`
        + `\t\theader_regexp ${matcher}_ip CF-Connecting-IP `,
      );
      expect(rendered).toContain(
        `\t\thandle @${matcher} {\n`
        + "\t\t\treverse_proxy unix//run/roost-edge/auth.sock {\n"
        + "\t\t\t\theader_up X-Forwarded-For {http.request.header.CF-Connecting-IP}\n"
        + "\t\t\t}\n"
        + "\t\t}",
      );
    }
    expect(rendered.match(/reverse_proxy unix\/\/run\/roost-edge\/auth[.]sock/g)).toHaveLength(8);
    expect(rendered).toContain(
      "\t@invalid_roost_auth path "
      + authEndpoints.map(([, , path]) => path).join(" "),
    );
    const connectingIpPattern = /header_regexp \S+_ip CF-Connecting-IP (\S+)/.exec(rendered)?.[1];
    expect(connectingIpPattern).toBeDefined();
    const connectingIp = new RegExp(connectingIpPattern!);
    for (const value of ["192.0.2.1", "2001:db8::1", "::1", "::ffff:192.0.2.1"]) {
      expect(connectingIp.test(value)).toBeTrue();
    }
    for (const value of ["", "999.0.0.1", "2001:::1", "192.0.2.1,198.51.100.1", " 192.0.2.1"]) {
      expect(connectingIp.test(value)).toBeFalse();
    }
    const firstAuthHandle = rendered.indexOf("handle @roost_auth_config");
    expect(firstAuthHandle).toBeGreaterThanOrEqual(0);
    expect(firstAuthHandle).toBeLessThan(rendered.indexOf("handle @prefixed_identity"));
    expect(rendered).not.toMatch(/handle_path \/(?:__roost|auth)\//);
    expect(rendered).not.toContain("/__roost/auth/*");
    expect(rendered).not.toContain("/__roost/signup/*");
    expect(rendered).not.toContain("/auth/google/*");
    expect(rendered).not.toContain("/run/roost-saas-private");
    expect(rendered).toContain(
      "\t\thandle /__roost/tenant/resolve {\n"
      + "\t\t\treverse_proxy unix//run/roost-edge/resolver.sock {\n"
      + "\t\t\t\theader_up X-Forwarded-For {http.request.header.CF-Connecting-IP}\n"
      + "\t\t\t}\n"
      + "\t\t}",
    );
    const firstHandle = `handle_path /_roost/t/${routeKey(FIRST_ID)}/*`;
    const secondHandle = `handle_path /_roost/t/${routeKey(SECOND_ID)}/*`;
    expect(rendered).toContain(
      `${firstHandle} {\n\t\t\treverse_proxy ${coordinatorContainerName(FIRST_ID)}:4104`,
    );
    expect(rendered).toContain(
      `${secondHandle} {\n\t\t\treverse_proxy ${coordinatorContainerName(SECOND_ID)}:4104`,
    );
    expect(rendered.indexOf(firstHandle)).toBeLessThan(rendered.indexOf(secondHandle));
    const identityHandle = rendered.indexOf("handle @prefixed_identity");
    const unknownHandle = rendered.indexOf("handle @unknown_tenant");
    expect(identityHandle).toBeGreaterThanOrEqual(0);
    expect(identityHandle).toBeLessThan(rendered.indexOf(firstHandle));
    expect(rendered).toContain(
      "@prefixed_identity path_regexp prefixed_identity "
      + "^/_roost/t/[^/]+/roost[.]v1[.]CoordinatorService/AuthCoordIdentity$",
    );
    expect(rendered).toContain(
      "@unknown_tenant path_regexp unknown_tenant ^/_roost/t/[^/]+(/.*)$",
    );
    expect(rendered).toContain(
      "rewrite * /roost.v1.CoordinatorService/AuthCoordIdentity\n"
      + "\t\t\treverse_proxy unix//run/roost-edge/legacy.sock",
    );
    expect(unknownHandle).toBeGreaterThan(rendered.indexOf(secondHandle));
    expect(rendered.slice(unknownHandle)).toStartWith(
      "handle @unknown_tenant {\n\t\t\trewrite * {re.unknown_tenant.1}\n"
      + "\t\t\treverse_proxy unix//run/roost-edge/legacy.sock",
    );
    expect(rendered).toEndWith(
      "\t\thandle {\n\t\t\treverse_proxy unix//run/roost-edge/legacy.sock\n\t\t}\n\t}\n}\n",
    );
    expect(rendered).not.toContain(coordinatorContainerName(disabledId));
    expect(rendered).not.toContain(coordinatorContainerName(reservedId));
    expect(renderCaddyTenantRoutes([])).not.toContain("handle_path /_roost/t/");
  });

  test("rejects duplicate routes, route-key collisions, malformed keys, and non-derived containers", () => {
    const row = coordinator(FIRST_ID, "routed");
    expect(() => renderCaddyTenantRoutes([row, { ...row }])).toThrow("duplicate");
    expect(renderCaddyTenantRoutes([{ ...row, hostname: "unused.invalid" }])).toBe(
      renderCaddyTenantRoutes([row]),
    );
    expect(() =>
      renderCaddyTenantRoutes([{ ...row, containerName: "other-container" }]),
    ).toThrow("container");
    expect(() =>
      renderCaddyTenantRoutes([{ ...row, routeKey: "A".repeat(64) }]),
    ).toThrow("route key");
    expect(() =>
      renderCaddyTenantRoutes([
        row,
        { ...coordinator(SECOND_ID, "active"), routeKey: row.routeKey },
      ]),
    ).toThrow("route-key collision");
    expect(() =>
      renderCaddyTenantRoutes([{ ...row, state: "unknown" as CoordinatorState }]),
    ).toThrow("state");
    expect(() =>
      renderCaddyTenantRoutes([{ ...row, id: "not-a-uuid" }]),
    ).toThrow("canonical UUID");
  });

  test("leaves the installed include byte-identical when candidate validation fails", async () => {
    const confDir = tempConfDir();
    const prior = Buffer.from([0x23, 0x20, 0x70, 0x72, 0x69, 0x6f, 0x72, 0x0a]);
    writeFileSync(finalPath(confDir), prior);
    const calls: string[][] = [];
    const router = new CaddyTenantRouter({
      confDir,
      runner: async (argv) => {
        calls.push([...argv]);
        return { exitCode: 7, stdout: "", stderr: "invalid" };
      },
    });

    await expect(router.reconcile([coordinator(FIRST_ID, "routed")])).rejects.toThrow(
      "Caddy validation failed with exit 7",
    );
    expect(readFileSync(finalPath(confDir))).toEqual(prior);
    expect(readdirSync(confDir)).toEqual(["roost-tenants.caddy"]);
    expect(calls).toHaveLength(1);
  });

  test("first install uses a non-imported candidate, exact argv, and the complete atomically installed include", async () => {
    const confDir = tempConfDir();
    const calls: string[][] = [];
    let candidateName = "";
    const rendered = renderCaddyTenantRoutes([coordinator(FIRST_ID, "routed")]);
    const router = new CaddyTenantRouter({
      confDir,
      runner: async (argv) => {
        calls.push([...argv]);
        if (argv[4] === "validate") {
          candidateName = basename(argv[6]!);
          expect(candidateName.endsWith(".caddy")).toBe(false);
          expect(readFileSync(join(confDir, candidateName), "utf8")).toBe(rendered);
          expect(readdirSync(confDir).some((name) => name.endsWith(".caddy"))).toBe(false);
        } else {
          expect(readFileSync(finalPath(confDir), "utf8")).toBe(rendered);
        }
        return ok();
      },
    });

    await router.reconcile([coordinator(FIRST_ID, "routed")]);

    expect(calls).toEqual([
      [
        "docker",
        "exec",
        "caddy",
        "caddy",
        "validate",
        "--config",
        `/etc/caddy/conf.d/${candidateName}`,
        "--adapter",
        "caddyfile",
      ],
      [
        "docker",
        "exec",
        "caddy",
        "caddy",
        "reload",
        "--config",
        "/etc/caddy/Caddyfile",
      ],
    ]);
    expect(readFileSync(finalPath(confDir), "utf8")).toBe(rendered);
    expect(readdirSync(confDir)).toEqual(["roost-tenants.caddy"]);
  });

  test("replacement presents the whole new file at reload and removes its candidate", async () => {
    const confDir = tempConfDir();
    const prior = renderCaddyTenantRoutes([coordinator(FIRST_ID, "routed")]);
    const replacement = renderCaddyTenantRoutes([coordinator(SECOND_ID, "active")]);
    writeFileSync(finalPath(confDir), prior);
    let calls = 0;
    const router = new CaddyTenantRouter({
      confDir,
      runner: async () => {
        calls += 1;
        if (calls === 1) expect(readFileSync(finalPath(confDir), "utf8")).toBe(prior);
        if (calls === 2) expect(readFileSync(finalPath(confDir), "utf8")).toBe(replacement);
        return ok();
      },
    });

    await router.reconcile([coordinator(SECOND_ID, "active")]);
    expect(readFileSync(finalPath(confDir), "utf8")).toBe(replacement);
    expect(readdirSync(confDir)).toEqual(["roost-tenants.caddy"]);
  });

  test("reload failure restores byte-identical prior content and reloads it before failing", async () => {
    const confDir = tempConfDir();
    const prior = Buffer.from("# deliberately noncanonical prior bytes\n\n", "utf8");
    writeFileSync(finalPath(confDir), prior);
    const calls: string[][] = [];
    const router = new CaddyTenantRouter({
      confDir,
      runner: async (argv) => {
        calls.push([...argv]);
        if (calls.length === 2) return { exitCode: 23, stdout: "", stderr: "reload broke" };
        if (calls.length === 3) expect(readFileSync(finalPath(confDir))).toEqual(prior);
        return ok();
      },
    });

    await expect(router.reconcile([coordinator(FIRST_ID, "invited")])).rejects.toThrow(
      "Caddy reload failed with exit 23",
    );
    expect(calls).toHaveLength(3);
    expect(calls[2]).toEqual(calls[1]);
    expect(readFileSync(finalPath(confDir))).toEqual(prior);
    expect(readdirSync(confDir)).toEqual(["roost-tenants.caddy"]);
  });

  test("failed first-install reload restores absence and reloads the prior base config", async () => {
    const confDir = tempConfDir();
    let calls = 0;
    const router = new CaddyTenantRouter({
      confDir,
      runner: async () => {
        calls += 1;
        if (calls === 2) return { exitCode: 8, stdout: "", stderr: "" };
        if (calls === 3) expect(readdirSync(confDir)).toEqual([]);
        return ok();
      },
    });

    await expect(router.reconcile([coordinator(FIRST_ID, "active")])).rejects.toThrow(
      "Caddy reload failed with exit 8",
    );
    expect(calls).toBe(3);
    expect(readdirSync(confDir)).toEqual([]);
  });

  test("reports both the primary and rollback reload failures while preserving prior bytes", async () => {
    const confDir = tempConfDir();
    const prior = Buffer.from("prior\u0000bytes\n", "utf8");
    writeFileSync(finalPath(confDir), prior);
    let calls = 0;
    const router = new CaddyTenantRouter({
      confDir,
      runner: async () => {
        calls += 1;
        if (calls === 2) return { exitCode: 31, stdout: "", stderr: "primary" };
        if (calls === 3) return { exitCode: 32, stdout: "", stderr: "rollback" };
        return ok();
      },
    });

    await expect(router.reconcile([coordinator(FIRST_ID, "active")])).rejects.toThrow(
      "Caddy reload failed with exit 31; rollback reload failed: Caddy rollback reload failed with exit 32",
    );
    expect(readFileSync(finalPath(confDir))).toEqual(prior);
    expect(readdirSync(confDir)).toEqual(["roost-tenants.caddy"]);
  });

  test("bounds injected command output before installing a candidate", async () => {
    const confDir = tempConfDir();
    const prior = Buffer.from("prior\n");
    writeFileSync(finalPath(confDir), prior);
    const router = new CaddyTenantRouter({
      confDir,
      runner: async () => ({ exitCode: 0, stdout: "x".repeat(65_537), stderr: "" }),
    });

    await expect(router.reconcile([coordinator(FIRST_ID, "active")])).rejects.toThrow(
      "command output exceeded its bound",
    );
    expect(readFileSync(finalPath(confDir))).toEqual(prior);
    expect(readdirSync(confDir)).toEqual(["roost-tenants.caddy"]);
  });
});
