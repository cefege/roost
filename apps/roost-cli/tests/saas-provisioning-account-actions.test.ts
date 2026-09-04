/**
 * These tests pin resend, disable, and enable transitions for provisioned accounts.
 * They stay separate from initial provisioning so each lifecycle concern remains scannable.
 * Shared fakes preserve the original effect-order and durable-state assertions.
 */
import { afterEach, describe, expect, test } from "bun:test";
import {
  ACCOUNT_ID,
  cleanupProvisioningFixtures,
  COORDINATOR_ID,
  fixture,
  IMAGE,
} from "./saas-provisioning-fixtures.ts";

afterEach(cleanupProvisioningFixtures);

describe("SaaS provisioning lifecycle", () => {
  test("expiry removes route before stop and resend produces one fresh invitation", async () => {
    const opened = fixture({ value: 1_000 });
    try {
      await opened.lifecycle.accountCreate("owner@example.com", IMAGE);
      opened.runtime.calls = [];
      opened.routes.calls = [];
      opened.runtime.status = { ...opened.runtime.status, expiresAtMs: 900 };
      const expired = await opened.lifecycle.reconcile();
      expect(expired[0]).toEqual(expect.objectContaining({ state: "failed", repaired: true }));
      expect(opened.routes.calls[0]).toBe("routes-reconcile");
      expect(opened.runtime.calls).toEqual([
        "ensure-container",
        "start-verify",
        "activation-status",
        "stop",
      ]);
      expect(opened.registry.getAccount(ACCOUNT_ID).state).toBe("pending");

      opened.runtime.calls = [];
      opened.runtime.status = { ...opened.runtime.status, expiresAtMs: 20_000 };
      const resent = await opened.lifecycle.accountResend("owner@example.com");
      expect(resent.coordinator.state).toBe("invited");
      expect(opened.runtime.calls.slice(0, 2)).toEqual(["stop", "seed"]);
      expect(opened.registry.listAccounts()).toHaveLength(1);
      expect(opened.registry.listCoordinators()).toHaveLength(1);
    } finally {
      opened.registry.close();
    }
  });

  test("resend stops every possible prior writer before recording reserved", async () => {
    const opened = fixture();
    try {
      await opened.lifecycle.accountCreate("owner@example.com", IMAGE);
      opened.runtime.calls = [];
      opened.runtime.failAt = "stop";
      await expect(opened.lifecycle.accountResend("owner@example.com")).rejects.toThrow("stop");
      expect(opened.registry.getCoordinator(COORDINATOR_ID).state).toBe("invited");
      expect(opened.runtime.calls).not.toContain("seed");

      opened.runtime.calls = [];
      opened.runtime.failAt = null;
      await opened.lifecycle.accountResend("owner@example.com");
      expect(opened.runtime.calls.indexOf("stop")).toBeLessThan(opened.runtime.calls.indexOf("seed"));
    } finally {
      opened.registry.close();
    }
  });

  test("disable removes the route before stop and enable verifies before restoring it", async () => {
    const opened = fixture();
    try {
      await opened.lifecycle.accountCreate("owner@example.com", IMAGE);
      opened.runtime.calls = [];
      opened.routes.calls = [];
      const disabled = await opened.lifecycle.accountDisable("owner@example.com");
      expect(disabled.account.state).toBe("disabled");
      expect(disabled.coordinator.state).toBe("disabled");
      expect(opened.routes.calls[0]).toBe("routes-reconcile");
      expect(opened.runtime.calls).toEqual(["stop"]);

      opened.runtime.calls = [];
      opened.routes.calls = [];
      opened.runtime.status = { ...opened.runtime.status, activated: false };
      const enabled = await opened.lifecycle.accountEnable("owner@example.com");
      expect(enabled.account.state).toBe("pending");
      expect(enabled.coordinator.state).toBe("invited");
      expect(opened.runtime.calls).toEqual([
        "stop",
        "seed",
        "ensure-container",
        "start-verify",
        "activation-status",
        "release-email",
      ]);
      expect(opened.routes.calls).toEqual([
        "routes-reconcile",
        "routes-verify",
        "resolver-verify",
      ]);
    } finally {
      opened.registry.close();
    }
  });

  test("enable rolls back route and container when route proof fails before registry restore", async () => {
    const opened = fixture();
    try {
      await opened.lifecycle.accountCreate("owner@example.com", IMAGE);
      await opened.lifecycle.accountDisable("owner@example.com");
      opened.runtime.calls = [];
      opened.routes.calls = [];
      opened.routes.desired = [];
      opened.routes.failVerify = true;

      await expect(opened.lifecycle.accountEnable("owner@example.com"))
        .rejects.toThrow("route verification");
      expect(opened.registry.getAccount(ACCOUNT_ID).state).toBe("disabled");
      expect(opened.registry.getCoordinator(COORDINATOR_ID).state).toBe("disabled");
      expect(opened.routes.desired.at(-1)).toEqual([]);
      expect(opened.runtime.calls.at(-1)).toBe("stop");
      expect(opened.runtime.calls).not.toContain("release-email");
    } finally {
      opened.registry.close();
    }
  });

  test("enable persists routed recovery before resolver proof and invited before release", async () => {
    const opened = fixture();
    try {
      await opened.lifecycle.accountCreate("owner@example.com", IMAGE);
      await opened.lifecycle.accountDisable("owner@example.com");
      opened.runtime.calls = [];
      opened.routes.failResolverVerify = true;

      await expect(opened.lifecycle.accountEnable("owner@example.com"))
        .rejects.toThrow("resolver verification");
      expect(opened.registry.getAccount(ACCOUNT_ID).state).toBe("pending");
      expect(opened.registry.getCoordinator(COORDINATOR_ID).state).toBe("routed");
      expect(opened.runtime.calls).not.toContain("release-email");

      opened.routes.failResolverVerify = false;
      opened.runtime.failAt = "release-email";
      await expect(opened.lifecycle.reconcile()).resolves.toEqual([
        expect.objectContaining({ state: "invited", repaired: true, error: expect.stringContaining("release-email") }),
      ]);
      expect(opened.registry.getCoordinator(COORDINATOR_ID).state).toBe("invited");

      opened.runtime.failAt = null;
      const reconciled = await opened.lifecycle.reconcile();
      expect(reconciled[0]).toEqual(expect.objectContaining({ state: "invited" }));
    } finally {
      opened.registry.close();
    }
  });
});
