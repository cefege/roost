import { describe, expect, test } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  activateManagedOwner,
  managedNewPasswordIssue,
  MANAGED_PASSWORD_RESET_ACKNOWLEDGEMENT,
  redeemManagedPasswordReset,
  requestManagedPasswordReset,
  type ManagedOwnerActivationDependencies,
  type ManagedPasswordResetRedeemDependencies,
  type ManagedPasswordResetRequestDependencies,
} from "../src/auth/managed-account.ts";

const VALID_PASSWORD = "twelve-chars";
const ROUTE_A = "a".repeat(64);

function resetRequestDependencies(
  requestReset: ManagedPasswordResetRequestDependencies["requestReset"],
): ManagedPasswordResetRequestDependencies {
  return {
    resolveRouteKey: async () => ROUTE_A,
    prepareRouteSwitch: async () => {},
    requestReset,
    persistRouteKey: () => true,
    clearStaleCredential: () => {},
  };
}


describe("managed owner activation", () => {
  test("enforces the shared 12–1024 character policy and exact confirmation", () => {
    const eleven = "a".repeat(11);
    const twelve = "a".repeat(12);
    const thousandTwentyFour = "a".repeat(1_024);
    const thousandTwentyFive = "a".repeat(1_025);

    expect(managedNewPasswordIssue(eleven, eleven)).toBe("too-short");
    expect(managedNewPasswordIssue(twelve, twelve)).toBeNull();
    expect(managedNewPasswordIssue(thousandTwentyFour, thousandTwentyFour)).toBeNull();
    expect(managedNewPasswordIssue(thousandTwentyFive, thousandTwentyFive)).toBe("too-long");
    expect(managedNewPasswordIssue(twelve, `${twelve}x`)).toBe("confirmation-mismatch");
  });

  test("orders key binding, activation, dashboard confirmation, and local authorization", async () => {
    const events: string[] = [];
    let confirmedDashboardId: string | null = null;
    const activationRequests: Array<Parameters<ManagedOwnerActivationDependencies["activateOwner"]>[1]> = [];
    const dependencies: ManagedOwnerActivationDependencies = {
      publicKeyB64: async () => {
        events.push("key");
        return "coordinator-origin-public-key";
      },
      activateOwner: async (routeKey, request) => {
        expect(routeKey).toBe(ROUTE_A);
        activationRequests.push(request);
        events.push("rpc");
        return { dashboardId: "coordinator-1" };
      },
      browserLabel: () => "Owner browser",
      confirmDashboard: async (routeKey, dashboardId) => {
        expect(routeKey).toBe(ROUTE_A);
        events.push(`dashboard:${dashboardId}`);
        confirmedDashboardId = dashboardId;
        return true;
      },
      confirmedDashboardId: () => confirmedDashboardId,
      markKeyAuthorized: () => events.push("authorized"),
      resumeBootstrap: () => events.push("resume"),
      clearCredential: (kind) => {
        events.push(`clear:${kind}`);
        return true;
      },
      replaceLocation: (path) => events.push(`replace:${path}`),
    };

    await expect(activateManagedOwner({
      routeKey: ROUTE_A,
      token: "activation-secret",
      password: VALID_PASSWORD,
      confirmation: VALID_PASSWORD,
    }, dependencies)).resolves.toEqual({ dashboardId: "coordinator-1" });

    expect(activationRequests[0]).toEqual({
      token: "activation-secret",
      newPassword: VALID_PASSWORD,
      sshPubkeyB64: "coordinator-origin-public-key",
      label: "Owner browser",
    });
    expect(events).toEqual([
      "key",
      "rpc",
      "dashboard:coordinator-1",
      "authorized",
      "resume",
      "clear:activation",
      "replace:/app",
    ]);
  });

  test("does not authorize, scrub, or enter the app without exact dashboard confirmation", async () => {
    const events: string[] = [];
    const dependencies: ManagedOwnerActivationDependencies = {
      publicKeyB64: async () => "public-key",
      activateOwner: async () => ({ dashboardId: "coordinator-1" }),
      browserLabel: () => "Owner browser",
      confirmDashboard: async () => true,
      confirmedDashboardId: () => "different-coordinator",
      markKeyAuthorized: () => events.push("authorized"),
      resumeBootstrap: () => events.push("resume"),
      clearCredential: (kind) => {
        events.push(`clear:${kind}`);
        return true;
      },
      replaceLocation: (path) => events.push(`replace:${path}`),
    };

    await expect(activateManagedOwner({
      routeKey: ROUTE_A,
      token: "activation-secret",
      password: VALID_PASSWORD,
      confirmation: VALID_PASSWORD,
    }, dependencies)).rejects.toThrow("no confirmed coordinator dashboard");
    expect(events).toEqual([]);
  });

  test("scrubs replayed or denied activation tokens but retains ambiguous credentials", async () => {
    async function runWith(error: unknown): Promise<string[]> {
      const events: string[] = [];
      const dependencies: ManagedOwnerActivationDependencies = {
        publicKeyB64: async () => "public-key",
        activateOwner: async () => { throw error; },
        browserLabel: () => "Owner browser",
        confirmDashboard: async () => false,
        confirmedDashboardId: () => null,
        markKeyAuthorized: () => events.push("authorized"),
        resumeBootstrap: () => events.push("resume"),
        clearCredential: (kind) => {
          events.push(`clear:${kind}`);
          return true;
        },
        replaceLocation: (path) => events.push(`replace:${path}`),
      };
      await expect(activateManagedOwner({
        routeKey: ROUTE_A,
        token: "activation-secret",
        password: VALID_PASSWORD,
        confirmation: VALID_PASSWORD,
      }, dependencies)).rejects.toBe(error);
      return events;
    }

    expect(await runWith(new ConnectError("unable to complete request", Code.PermissionDenied)))
      .toEqual(["clear:activation"]);
    expect(await runWith(new ConnectError("connection lost", Code.Unavailable))).toEqual([]);
  });
});

describe("managed password recovery", () => {
  test("returns one acknowledgement for accepted and failed reset requests", async () => {
    const requestedEmails: string[] = [];
    const accepted = await requestManagedPasswordReset(
      " owner@example.com ",
      resetRequestDependencies(async (routeKey, { email }) => {
        expect(routeKey).toBe(ROUTE_A);
        requestedEmails.push(email);
        return {};
      }),
    );
    const failed = await requestManagedPasswordReset(
      "unknown@example.com",
      resetRequestDependencies(async (_routeKey, { email }) => {
        requestedEmails.push(email);
        throw new TypeError("offline");
      }),
    );

    expect(accepted).toBe(MANAGED_PASSWORD_RESET_ACKNOWLEDGEMENT);
    expect(failed).toBe(MANAGED_PASSWORD_RESET_ACKNOWLEDGEMENT);
    expect(requestedEmails).toEqual(["owner@example.com", "unknown@example.com"]);
  });

  test("destroys account state and every current, staged, and cached key only after reset confirmation", async () => {
    const events: string[] = [];
    const identity = { account: true, currentKey: true, stagedKey: true, cachedJwt: true };
    const dependencies: ManagedPasswordResetRedeemDependencies = {
      redeemReset: async (routeKey, request) => {
        expect(routeKey).toBe(ROUTE_A);
        events.push(`rpc:${request.token}:${request.newPassword}`);
        return { ok: true };
      },
      clearClientState: () => {
        events.push("clear-client");
        identity.account = false;
      },
      clearWebKeyMaterial: async () => {
        events.push("clear-keys");
        identity.currentKey = false;
        identity.stagedKey = false;
        identity.cachedJwt = false;
      },
      clearCredential: (kind) => {
        events.push(`clear:${kind}`);
        return true;
      },
      replaceLocation: (path) => events.push(`replace:${path}`),
    };

    await redeemManagedPasswordReset({
      routeKey: ROUTE_A,
      token: "reset-secret",
      password: VALID_PASSWORD,
      confirmation: VALID_PASSWORD,
    }, dependencies);

    expect(identity).toEqual({ account: false, currentKey: false, stagedKey: false, cachedJwt: false });
    expect(events).toEqual([
      `rpc:reset-secret:${VALID_PASSWORD}`,
      "clear:reset",
      "clear-client",
      "clear-keys",
      "replace:/login",
    ]);
  });

  test("denial scrubs only the spent reset token while ambiguity preserves token and identity", async () => {
    async function runWith(error: unknown): Promise<{ events: string[]; identityPresent: boolean }> {
      const events: string[] = [];
      let identityPresent = true;
      const dependencies: ManagedPasswordResetRedeemDependencies = {
        redeemReset: async () => { throw error; },
        clearClientState: () => {
          events.push("clear-client");
          identityPresent = false;
        },
        clearWebKeyMaterial: async () => {
          events.push("clear-keys");
          identityPresent = false;
        },
        clearCredential: (kind) => {
          events.push(`clear:${kind}`);
          return true;
        },
        replaceLocation: (path) => events.push(`replace:${path}`),
      };
      await expect(redeemManagedPasswordReset({
        routeKey: ROUTE_A,
        token: "reset-secret",
        password: VALID_PASSWORD,
        confirmation: VALID_PASSWORD,
      }, dependencies)).rejects.toBe(error);
      return { events, identityPresent };
    }

    expect(await runWith(new ConnectError("unable to complete request", Code.PermissionDenied)))
      .toEqual({ events: ["clear:reset"], identityPresent: true });
    expect(await runWith(new ConnectError("connection lost", Code.Unavailable)))
      .toEqual({ events: [], identityPresent: true });
  });

  test("local password rejection cannot call reset or clear browser identity", async () => {
    const events: string[] = [];
    const dependencies: ManagedPasswordResetRedeemDependencies = {
      redeemReset: async () => {
        events.push("rpc");
        return { ok: true };
      },
      clearClientState: () => events.push("clear-client"),
      clearWebKeyMaterial: async () => { events.push("clear-keys"); },
      clearCredential: (kind) => {
        events.push(`clear:${kind}`);
        return true;
      },
      replaceLocation: (path) => events.push(`replace:${path}`),
    };

    await expect(redeemManagedPasswordReset({
      routeKey: ROUTE_A,
      token: "reset-secret",
      password: "elevenchars",
      confirmation: "elevenchars",
    }, dependencies)).rejects.toThrow("at least 12");
    expect(events).toEqual([]);
  });
});
