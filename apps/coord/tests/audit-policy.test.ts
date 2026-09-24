// Audit policy tests cover pure storage predicates and the Connect interceptor
// method fence. Pair polling has no durable audit outcome; successful PairConfirm
// is reserved for the handler's requester-identity audit row.

import { describe, expect, test } from "bun:test";
import {
  _shouldPersistMethodAudit,
} from "../src/auth/auth-interceptor.ts";
import {
  recordAuditTelemetry,
  shouldPersistConnectAudit,
  shouldPersistNonConnectAudit,
  SPA_AUDIT_TELEMETRY_PATH,
} from "../src/middleware/security.ts";
import { getMetricsSnapshot } from "../src/diagnostics/telemetry.ts";

describe("audit persistence policy", () => {
  test("skips only successful SPA/static reads", () => {
    expect(shouldPersistNonConnectAudit({
      surface: "spa",
      method: "GET",
      status: 200,
    })).toBe(false);
    expect(shouldPersistNonConnectAudit({
      surface: "spa",
      method: "HEAD",
      status: 304,
    })).toBe(false);
    expect(shouldPersistNonConnectAudit({
      surface: "spa",
      method: "POST",
      status: 200,
    })).toBe(true);
    expect(shouldPersistNonConnectAudit({
      surface: "spa",
      method: "GET",
      status: 404,
    })).toBe(true);
    expect(shouldPersistNonConnectAudit({
      surface: "db-export",
      method: "GET",
      status: 200,
    })).toBe(true);
    expect(shouldPersistNonConnectAudit({
      surface: "api",
      method: "GET",
      status: 200,
    })).toBe(true);
  });

  test("skips only an anonymous 401 that arrived through a trusted proxy", () => {
    expect(shouldPersistConnectAudit({
      listener: "trusted-proxy",
      status: 401,
      callerFp: null,
    })).toBe(false);
    expect(shouldPersistConnectAudit({
      listener: "trusted-proxy",
      status: 401,
      callerFp: "device-fingerprint",
    })).toBe(true);
    expect(shouldPersistConnectAudit({
      listener: "trusted-proxy",
      status: 403,
      callerFp: null,
    })).toBe(true);
    expect(shouldPersistConnectAudit({
      listener: "direct",
      status: 401,
      callerFp: null,
    })).toBe(true);
  });

  test("records skipped static reads under one bounded telemetry label", () => {
    const before = getMetricsSnapshot();
    const requestCount = before.requests[SPA_AUDIT_TELEMETRY_PATH] ?? 0;
    const errorCount = before.errors[SPA_AUDIT_TELEMETRY_PATH] ?? 0;
    recordAuditTelemetry(SPA_AUDIT_TELEMETRY_PATH, 200);
    recordAuditTelemetry(SPA_AUDIT_TELEMETRY_PATH, 304);
    const after = getMetricsSnapshot();
    expect(after.requests[SPA_AUDIT_TELEMETRY_PATH]).toBe(requestCount + 2);
    expect(after.errors[SPA_AUDIT_TELEMETRY_PATH] ?? 0).toBe(errorCount);
  });

  test("never persists PairPoll and skips only successful PairConfirm", () => {
    const pollPath = "/roost.v1.CoordinatorService/PairPoll";
    const before = getMetricsSnapshot();
    const requestCount = before.requests[pollPath] ?? 0;
    const errorCount = before.errors[pollPath] ?? 0;
    recordAuditTelemetry(pollPath, 200);
    recordAuditTelemetry(pollPath, 404);
    const after = getMetricsSnapshot();
    expect(after.requests[pollPath]).toBe(requestCount + 2);
    expect(after.errors[pollPath]).toBe(errorCount + 1);
    expect(_shouldPersistMethodAudit("PairPoll", 200)).toBe(false);
    expect(_shouldPersistMethodAudit("PairPoll", 404)).toBe(false);
    expect(_shouldPersistMethodAudit("PairPoll", 500)).toBe(false);
    expect(_shouldPersistMethodAudit("PairConfirm", 200)).toBe(false);
    expect(_shouldPersistMethodAudit("PairConfirm", 200, true)).toBe(true);
    expect(_shouldPersistMethodAudit("PairConfirm", 412)).toBe(true);
  });

  test("skips successful approver status polls but keeps their failures", () => {
    expect(_shouldPersistMethodAudit("PairApprovalStatus", 200)).toBe(false);
    expect(_shouldPersistMethodAudit("PairApprovalStatus", 401)).toBe(true);
    expect(_shouldPersistMethodAudit("PairApprovalStatus", 404)).toBe(true);
    expect(_shouldPersistMethodAudit("PairApprovalStatus", 500)).toBe(true);
  });
});
