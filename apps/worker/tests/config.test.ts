// Worker configuration contract for the opt-in conversation restore gate.
// These tests keep its environment syntax and platform boundary independent
// from boot reconciliation and the restore implementation.

import { describe, expect, test } from "bun:test";
import { DEFAULT_COORDINATOR_BIND } from "@roost/shared/config";
import { loadWorkerConfig } from "../src/config.ts";

const BASE_ENV = {
  ROOST_WORKER_DATA_DIR: "/tmp/roost-worker-config-test",
  ROOST_WORKER_LOG_DIR: "/tmp/roost-worker-config-test/logs",
};

describe("coordinatorUrl", () => {
  test("falls back to the coordinator's own default bind, not a second literal", () => {
    // A retired port here made `roost deploy` fail keeper admission with
    // ECONNREFUSED while the worker itself was healthy.
    expect(loadWorkerConfig(BASE_ENV).coordinatorUrl)
      .toBe(`http://${DEFAULT_COORDINATOR_BIND}`);
    expect(loadWorkerConfig({ ...BASE_ENV, ROOST_COORDINATOR_URL: "https://roost.example.com" })
      .coordinatorUrl).toBe("https://roost.example.com");
  });
});

describe("ROOST_AGENT_CONVERSATION_RESTORE", () => {
  test("defaults to disabled on every platform when absent", () => {
    for (const platform of ["linux", "darwin", "win32"] as const) {
      expect(loadWorkerConfig(BASE_ENV, platform).agentConversationRestore).toBe(false);
    }
  });

  test("accepts only exact 0 and 1 values on POSIX", () => {
    for (const platform of ["linux", "darwin"] as const) {
      expect(loadWorkerConfig({
        ...BASE_ENV,
        ROOST_AGENT_CONVERSATION_RESTORE: "0",
      }, platform).agentConversationRestore).toBe(false);
      expect(loadWorkerConfig({
        ...BASE_ENV,
        ROOST_AGENT_CONVERSATION_RESTORE: "1",
      }, platform).agentConversationRestore).toBe(true);
    }
  });

  test("rejects every other explicit value", () => {
    for (const value of ["", "2", "true", "01", " 1 "]) {
      expect(() => loadWorkerConfig({
        ...BASE_ENV,
        ROOST_AGENT_CONVERSATION_RESTORE: value,
      }, "linux")).toThrow("ROOST_AGENT_CONVERSATION_RESTORE must be exactly 0 or 1");
    }
  });

  test("rejects explicit enablement on Windows", () => {
    expect(() => loadWorkerConfig({
      ...BASE_ENV,
      ROOST_AGENT_CONVERSATION_RESTORE: "1",
    }, "win32")).toThrow("ROOST_AGENT_CONVERSATION_RESTORE=1 is unsupported on Windows");
    expect(loadWorkerConfig({
      ...BASE_ENV,
      ROOST_AGENT_CONVERSATION_RESTORE: "0",
    }, "win32").agentConversationRestore).toBe(false);
  });
});

describe("ROOST_WORKER_TERMINAL_CAP", () => {
  test("accepts a strict nonnegative decimal cap", () => {
    expect(loadWorkerConfig(BASE_ENV).terminalCoreCap).toBeUndefined();
    expect(loadWorkerConfig({
      ...BASE_ENV,
      ROOST_WORKER_TERMINAL_CAP: "0",
    }).terminalCoreCap).toBe(0);
    expect(loadWorkerConfig({
      ...BASE_ENV,
      ROOST_WORKER_TERMINAL_CAP: "17",
    }).terminalCoreCap).toBe(17);
  });

  test("rejects whitespace, signs, fractions, and unsafe values", () => {
    for (const value of ["", "-1", "+1", "01", "1.5", " 1", "1 ", "1e2", "4294967296", "9007199254740992"]) {
      expect(() => loadWorkerConfig({
        ...BASE_ENV,
        ROOST_WORKER_TERMINAL_CAP: value,
      })).toThrow("ROOST_WORKER_TERMINAL_CAP must be a nonnegative decimal integer");
    }
  });
});
