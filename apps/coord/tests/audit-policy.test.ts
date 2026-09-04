import { describe, expect, test } from "bun:test";
import { getMetricsSnapshot } from "../src/telemetry.ts";
import {
  recordAuditTelemetry,
  shouldPersistConnectAudit,
  shouldPersistNonConnectAudit,
  SPA_AUDIT_TELEMETRY_PATH,
} from "../src/middleware/security.ts";

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

  test("skips only anonymous public-edge 401 rows", () => {
    expect(shouldPersistConnectAudit({
      listener: "public-edge",
      status: 401,
      callerFp: null,
    })).toBe(false);
    expect(shouldPersistConnectAudit({
      listener: "public-edge",
      status: 401,
      callerFp: "device-fingerprint",
    })).toBe(true);
    expect(shouldPersistConnectAudit({
      listener: "direct",
      status: 401,
      callerFp: null,
    })).toBe(true);
    expect(shouldPersistConnectAudit({
      listener: "public-edge",
      status: 403,
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
});
