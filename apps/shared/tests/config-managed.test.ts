// Covers managed-container profiles, SaaS email settings, and file-backed secrets.
// The suite drives loadCoordConfig through complete and adversarial environment shapes.
// Shared fixtures keep temporary secret files isolated and cleaned after every test.

import { afterEach, describe, expect, test } from "bun:test";
import { join } from "node:path";
import { loadCoordConfig } from "../src/config.ts";
import {
  AUD,
  cleanConfigTestWorkdirs,
  createConfigTestWorkdir,
  INSTANCE_ID,
  managedContainerEnv,
  MANAGED_ORIGIN,
  OUTBOX_KEY,
  TENANT_ROUTE_KEY,
  writeSecretFiles,
} from "./config-test-fixtures.ts";

afterEach(cleanConfigTestWorkdirs);

describe("coordinator managed and email configuration", () => {
  const emailEnv = {
    ROOST_RESEND_ENDPOINT: "https://api.resend.com/emails",
    ROOST_RESEND_API_KEY: "re_test",
    ROOST_EMAIL_FROM: "Roost <noreply@example.com>",
    ROOST_EMAIL_OUTBOX_KEY: OUTBOX_KEY,
  };
  const managedSaasEnv = {
    ROOST_SAAS_MODE: "1",
    ROOST_WEB_PUBLIC_URL: "https://roost.example.com",
  };

  test("keeps generic managed SaaS email-optional", () => {
    const cfg = loadCoordConfig(managedSaasEnv);
    expect(cfg.saasMode).toBe(true);
    expect(cfg.managedContainer).toBe(false);
    expect(cfg.webPublicUrl).toBe("https://roost.example.com");
    expect(cfg.resendEndpoint).toBeUndefined();
    expect(cfg.resendApiKey).toBeUndefined();
    expect(cfg.emailFrom).toBeUndefined();
    expect(cfg.emailOutboxKey).toBeUndefined();
  });

  test("accepts only the exact complete managed-container profile", () => {
    expect(loadCoordConfig({}).managedContainer).toBe(false);
    expect(loadCoordConfig({ ROOST_MANAGED_CONTAINER: "0" }).managedContainer).toBe(false);

    const cfg = loadCoordConfig(managedContainerEnv());
    expect(cfg).toMatchObject({
      managedContainer: true,
      saasMode: true,
      instanceId: INSTANCE_ID,
      tenantRouteKey: TENANT_ROUTE_KEY,
      bind: "127.0.0.1:4103",
      publicBind: "0.0.0.0:4104",
      trustProxy: true,
      webPublicUrl: MANAGED_ORIGIN,
      resendEndpoint: "https://api.resend.com/emails",
      resendApiKey: "re_file_secret",
      emailFrom: "Roost <noreply@roosttt.com>",
      emailOutboxKey: OUTBOX_KEY,
      saasAuthVerifyKeyPath: expect.stringContaining("/saas-auth-verify-key"),
    });
    expect(cfg.cfAccessTeamDomain).toBeUndefined();
    expect(cfg.cfAccessAud).toBeUndefined();
    expect(cfg.tlsCertPath).toBeUndefined();
    expect(cfg.tlsKeyPath).toBeUndefined();
    expect(cfg.publicUrl).toBeUndefined();
  });

  test("requires every managed-container profile setting", () => {
    for (const [name, value] of [
      ["ROOST_SAAS_MODE", undefined],
      ["ROOST_COORDINATOR_INSTANCE_ID", undefined],
      ["ROOST_TENANT_ROUTE_KEY", undefined],
      ["ROOST_COORDINATOR_BIND", undefined],
      ["ROOST_PUBLIC_BIND", undefined],
      ["ROOST_TRUST_PROXY", undefined],
      ["ROOST_WEB_PUBLIC_URL", undefined],
      ["ROOST_RESEND_ENDPOINT", undefined],
      ["ROOST_RESEND_API_KEY_FILE", undefined],
      ["ROOST_EMAIL_FROM", undefined],
      ["ROOST_EMAIL_OUTBOX_KEY_FILE", undefined],
      ["ROOST_SAAS_AUTH_VERIFY_KEY_FILE", undefined],
    ] as const) {
      expect(
        () => loadCoordConfig(managedContainerEnv({ [name]: value })),
        name,
      ).toThrow();
    }
  });

  test("requires a canonical lowercase instance UUID", () => {
    for (const instanceId of [
      "11111111111141118111111111111111",
      "11111111-1111-0111-8111-111111111111",
      "11111111-1111-4111-7111-111111111111",
      "11111111-1111-4111-8111-11111111111Z",
      "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
    ]) {
      expect(
        () => loadCoordConfig(managedContainerEnv({
          ROOST_COORDINATOR_INSTANCE_ID: instanceId,
        })),
        instanceId,
      ).toThrow("canonical lowercase UUID");
    }
    expect(() => loadCoordConfig({
      ROOST_COORDINATOR_INSTANCE_ID: INSTANCE_ID,
    })).toThrow("requires ROOST_MANAGED_CONTAINER=1");
  });

  test("requires an exact managed tenant route key and rejects it outside the profile", () => {
    for (const routeKey of [
      "",
      "a".repeat(63),
      "a".repeat(65),
      "A".repeat(64),
      `${"a".repeat(63)}g`,
      ` ${"a".repeat(64)}`,
    ]) {
      expect(
        () => loadCoordConfig(managedContainerEnv({
          ROOST_TENANT_ROUTE_KEY: routeKey,
        })),
        JSON.stringify(routeKey),
      ).toThrow("exactly 64 lowercase hex characters");
    }
    expect(() => loadCoordConfig({
      ROOST_TENANT_ROUTE_KEY: TENANT_ROUTE_KEY,
    })).toThrow("requires ROOST_MANAGED_CONTAINER=1");
  });

  test("requires an absolute SaaS auth verify-key file only in managed containers", () => {
    expect(() => loadCoordConfig(managedContainerEnv({
      ROOST_SAAS_AUTH_VERIFY_KEY_FILE: "relative/verify-key",
    }))).toThrow("ROOST_SAAS_AUTH_VERIFY_KEY_FILE must be an absolute path");
    expect(() => loadCoordConfig({
      ROOST_SAAS_AUTH_VERIFY_KEY_FILE: "/run/secrets/saas-auth-verify-key",
    })).toThrow("requires ROOST_MANAGED_CONTAINER=1");
    expect(loadCoordConfig({}).saasAuthVerifyKeyPath).toBeUndefined();
  });

  test("pins managed listeners and browser origin exactly", () => {
    for (const overrides of [
      { ROOST_COORDINATOR_BIND: "127.0.0.1:4102" },
      { ROOST_COORDINATOR_BIND: "0.0.0.0:4103" },
      { ROOST_PUBLIC_BIND: "127.0.0.1:4104" },
      { ROOST_PUBLIC_BIND: "0.0.0.0:4105" },
      { ROOST_WEB_PUBLIC_URL: `${MANAGED_ORIGIN}/` },
      { ROOST_WEB_PUBLIC_URL: `${MANAGED_ORIGIN}:443` },
      { ROOST_WEB_PUBLIC_URL: `${MANAGED_ORIGIN}/login` },
      { ROOST_WEB_PUBLIC_URL: `${MANAGED_ORIGIN}?tenant=secret` },
      { ROOST_WEB_PUBLIC_URL: `${MANAGED_ORIGIN}#secret` },
      {
        ROOST_WEB_PUBLIC_URL:
          `https://c-${INSTANCE_ID.replaceAll("-", "")}.dashboard.roosttt.com`,
      },
    ]) {
      expect(() => loadCoordConfig(managedContainerEnv(overrides)), JSON.stringify(overrides))
        .toThrow();
    }
  });

  test("forbids edge access, direct TLS, and a coordinator public URL", () => {
    for (const overrides of [
      {
        ROOST_CF_ACCESS_TEAM_DOMAIN: "example.cloudflareaccess.com",
        ROOST_CF_ACCESS_AUD: AUD,
      },
      { ROOST_TLS_CERT_PATH: "/run/secrets/tls-cert" },
      { ROOST_TLS_KEY_PATH: "/run/secrets/tls-key" },
      { ROOST_COORDINATOR_PUBLIC_URL: "https://private.example.ts.net:4102" },
    ]) {
      expect(() => loadCoordConfig(managedContainerEnv(overrides)), JSON.stringify(overrides))
        .toThrow();
    }
  });

  test("requires managed secrets to come only from files", () => {
    expect(() => loadCoordConfig(managedContainerEnv({
      ROOST_RESEND_API_KEY: "re_direct",
    }))).toThrow("cannot both be configured");
    expect(() => loadCoordConfig(managedContainerEnv({
      ROOST_EMAIL_OUTBOX_KEY: OUTBOX_KEY,
    }))).toThrow("cannot both be configured");
    expect(() => loadCoordConfig(managedContainerEnv({
      ROOST_RESEND_API_KEY_FILE: undefined,
      ROOST_RESEND_API_KEY: "re_direct",
    }))).toThrow("requires Resend and email outbox secrets through *_FILE");
    expect(() => loadCoordConfig(managedContainerEnv({
      ROOST_EMAIL_OUTBOX_KEY_FILE: undefined,
      ROOST_EMAIL_OUTBOX_KEY: OUTBOX_KEY,
    }))).toThrow("requires Resend and email outbox secrets through *_FILE");
  });

  test("requires a valid public browser origin in generic SaaS mode", () => {
    expect(() => loadCoordConfig({ ROOST_SAAS_MODE: "1" }))
      .toThrow("ROOST_SAAS_MODE=1 requires ROOST_WEB_PUBLIC_URL");
    expect(() => loadCoordConfig({
      ...managedSaasEnv,
      ROOST_WEB_PUBLIC_URL: "http://roost.example.com",
    })).toThrow("ROOST_WEB_PUBLIC_URL must be an HTTPS origin");
    expect(() => loadCoordConfig({
      ...managedSaasEnv,
      ROOST_WEB_PUBLIC_URL: "https://roost.example.com/login",
    })).toThrow("ROOST_WEB_PUBLIC_URL must be an HTTPS origin");
  });

  test("requires the complete email configuration group in every generic mode", () => {
    for (const key of Object.keys(emailEnv)) {
      for (const modeEnv of [{}, managedSaasEnv]) {
        expect(() => loadCoordConfig({ ...modeEnv, ...emailEnv, [key]: undefined }), key)
          .toThrow("must all be configured together");
      }
    }
    expect(() => loadCoordConfig({ ROOST_RESEND_ENDPOINT: "" }))
      .toThrow("must all be configured together");
  });

  test("keeps direct email secrets supported in generic modes", () => {
    const cfg = loadCoordConfig(emailEnv);
    expect(cfg.saasMode).toBe(false);
    expect(cfg.resendEndpoint).toBe("https://api.resend.com/emails");
    expect(cfg.resendApiKey).toBe("re_test");
    expect(cfg.emailFrom).toBe("Roost <noreply@example.com>");
    expect(cfg.emailOutboxKey).toBe(OUTBOX_KEY);
  });

  test("loads newline-terminated file secrets in generic modes", () => {
    const { resendPath, outboxPath } = writeSecretFiles();
    const cfg = loadCoordConfig({
      ROOST_RESEND_ENDPOINT: "https://api.resend.com/emails",
      ROOST_RESEND_API_KEY_FILE: resendPath,
      ROOST_EMAIL_FROM: "Roost <noreply@example.com>",
      ROOST_EMAIL_OUTBOX_KEY_FILE: outboxPath,
    });
    expect(cfg.resendApiKey).toBe("re_file_secret");
    expect(cfg.emailOutboxKey).toBe(OUTBOX_KEY);
  });

  test("rejects direct and file secret ambiguity in generic modes", () => {
    const { resendPath, outboxPath } = writeSecretFiles();
    expect(() => loadCoordConfig({
      ...emailEnv,
      ROOST_RESEND_API_KEY_FILE: resendPath,
    })).toThrow("ROOST_RESEND_API_KEY and ROOST_RESEND_API_KEY_FILE cannot both");
    expect(() => loadCoordConfig({
      ...emailEnv,
      ROOST_EMAIL_OUTBOX_KEY_FILE: outboxPath,
    })).toThrow("ROOST_EMAIL_OUTBOX_KEY and ROOST_EMAIL_OUTBOX_KEY_FILE cannot both");
  });

  test("rejects empty, oversized, non-file, and unreadable secret files", () => {
    const empty = writeSecretFiles({ resend: " \n" });
    expect(() => loadCoordConfig({
      ROOST_RESEND_API_KEY_FILE: empty.resendPath,
    })).toThrow("must not be empty");

    const oversized = writeSecretFiles({
      resend: Buffer.alloc(64 * 1024 + 1, "x"),
    });
    expect(() => loadCoordConfig({
      ROOST_RESEND_API_KEY_FILE: oversized.resendPath,
    })).toThrow("65536-byte limit");

    const directory = createConfigTestWorkdir("roost-config-secret-dir-");
    expect(() => loadCoordConfig({
      ROOST_RESEND_API_KEY_FILE: directory,
    })).toThrow("readable regular file");
    expect(() => loadCoordConfig({
      ROOST_RESEND_API_KEY_FILE: join(directory, "missing"),
    })).toThrow("readable regular file");
  });

  test("validates resolved email settings", () => {
    expect(() => loadCoordConfig({
      ...emailEnv,
      ROOST_RESEND_ENDPOINT: "http://api.resend.com/emails",
    })).toThrow("ROOST_RESEND_ENDPOINT");
    expect(() => loadCoordConfig({
      ...emailEnv,
      ROOST_EMAIL_OUTBOX_KEY: "not-a-key",
    })).toThrow("ROOST_EMAIL_OUTBOX_KEY");
    expect(() => loadCoordConfig({
      ...emailEnv,
      ROOST_EMAIL_FROM: " ",
    })).toThrow("must all be non-empty");
  });
});
