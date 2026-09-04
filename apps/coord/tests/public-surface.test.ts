/**
 * WHY: This suite owns the exact managed route-classification allowlist for the public edge.
 * Bun discovers it to pin anonymous, protected, worker, SPA, and Sync route categories.
 * It depends directly on the production classifier and shared coordinator RPC prefix.
 */
import { describe, expect, test } from "bun:test";
import { classifyManagedPublicRoute } from "../src/middleware/public-surface.ts";
import { RPC } from "./public-surface-harness.ts";

describe("managed public route classification", () => {
  test("admits only exact public auth, protected browser, worker onboarding, SPA, and Sync paths", () => {
    expect(classifyManagedPublicRoute("/login", "GET")).toBe("spa");
    expect(classifyManagedPublicRoute("/assets/app.js", "GET")).toBe("spa");
    expect(classifyManagedPublicRoute("/login", "POST")).toBe("denied");
    expect(classifyManagedPublicRoute(`${RPC}AuthCoordIdentity`, "POST"))
      .toBe("public-auth-rpc");
    expect(classifyManagedPublicRoute(`${RPC}AuthPasswordLogin`, "POST"))
      .toBe("public-auth-rpc");
    expect(classifyManagedPublicRoute(`${RPC}AuthPasswordResetRequest`, "POST"))
      .toBe("public-auth-rpc");
    expect(classifyManagedPublicRoute(`${RPC}AuthPasswordResetRedeem`, "POST"))
      .toBe("public-auth-rpc");
    expect(classifyManagedPublicRoute(`${RPC}AuthFederatedContinue`, "POST"))
      .toBe("public-auth-rpc");
    expect(classifyManagedPublicRoute(`${RPC}AuthPasswordLogin/extra`, "POST"))
      .toBe("denied");
    expect(classifyManagedPublicRoute(`${RPC}SessionsList`, "POST"))
      .toBe("protected-rpc");
    expect(classifyManagedPublicRoute(`${RPC}AuthMintBootstrap`, "POST"))
      .toBe("protected-rpc");
    expect(classifyManagedPublicRoute(`${RPC}AuthCredentialsGet`, "POST"))
      .toBe("protected-rpc");
    expect(classifyManagedPublicRoute(`${RPC}AuthPasswordAdd`, "POST"))
      .toBe("protected-rpc");
    expect(classifyManagedPublicRoute(`${RPC}AuthFederatedLinkBegin`, "POST"))
      .toBe("protected-rpc");
    expect(classifyManagedPublicRoute(`${RPC}AuthFederatedLink`, "POST"))
      .toBe("protected-rpc");
    expect(classifyManagedPublicRoute(`${RPC}AuthRedeemWorker`, "POST"))
      .toBe("worker-redeem-rpc");
    expect(classifyManagedPublicRoute(`${RPC}WorkersRegister`, "POST"))
      .toBe("worker-rpc");
    expect(classifyManagedPublicRoute(`${RPC}WorkersHeartbeat`, "POST"))
      .toBe("worker-rpc");
    expect(classifyManagedPublicRoute(`${RPC}WorkersDeployStart`, "POST"))
      .toBe("denied");
    expect(classifyManagedPublicRoute(`${RPC}WorkersDeployOutput`, "POST"))
      .toBe("denied");
    expect(classifyManagedPublicRoute(`${RPC}FuturePrivilegedMethod`, "POST"))
      .toBe("denied");
    expect(classifyManagedPublicRoute("/ws/coord-sync", "GET")).toBe("sync");
    expect(classifyManagedPublicRoute(`/ws/coord-worker/${"a".repeat(64)}`, "GET"))
      .toBe("worker");
    expect(classifyManagedPublicRoute("/ws/coord-worker/fingerprint", "GET"))
      .toBe("denied");
  });
});
