// Worker configuration contract for terminal peer settings and the opt-in
// conversation restore gate. These tests keep environment syntax and platform
// boundaries independent from boot reconciliation.

import { describe, expect, test } from "bun:test";
import { DEFAULT_COORDINATOR_BIND } from "@roost/host/config";
import { loadWorkerConfig } from "../../src/host/config.ts";

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

describe("ROOST_TERMINAL_PEER_ENABLED", () => {
  test("defaults by worker platform", () => {
    expect(loadWorkerConfig(BASE_ENV, "linux").terminalPeerEnabled).toBe(true);
    expect(loadWorkerConfig(BASE_ENV, "darwin").terminalPeerEnabled).toBe(true);
    expect(loadWorkerConfig(BASE_ENV, "win32").terminalPeerEnabled).toBe(false);
  });

  test("accepts only exact operator values on POSIX", () => {
    for (const platform of ["linux", "darwin"] as const) {
      expect(loadWorkerConfig({
        ...BASE_ENV,
        ROOST_TERMINAL_PEER_ENABLED: "0",
      }, platform).terminalPeerEnabled).toBe(false);
      expect(loadWorkerConfig({
        ...BASE_ENV,
        ROOST_TERMINAL_PEER_ENABLED: "1",
      }, platform).terminalPeerEnabled).toBe(true);
    }
  });

  test("rejects every other explicit value", () => {
    for (const value of ["", "2", "true", "01", " 1 "]) {
      expect(() => loadWorkerConfig({
        ...BASE_ENV,
        ROOST_TERMINAL_PEER_ENABLED: value,
      }, "linux")).toThrow("ROOST_TERMINAL_PEER_ENABLED must be exactly 0 or 1");
    }
  });

  test("rejects explicit enablement on Windows", () => {
    expect(() => loadWorkerConfig({
      ...BASE_ENV,
      ROOST_TERMINAL_PEER_ENABLED: "1",
    }, "win32")).toThrow("ROOST_TERMINAL_PEER_ENABLED=1 is unsupported on Windows");
    expect(loadWorkerConfig({
      ...BASE_ENV,
      ROOST_TERMINAL_PEER_ENABLED: "0",
    }, "win32").terminalPeerEnabled).toBe(false);
  });
});

describe("ROOST_TERMINAL_PEER_BIND_ADDRESS", () => {
  test("preserves optional literal unicast addresses", () => {
    expect(loadWorkerConfig(BASE_ENV).terminalPeerBindAddress).toBeUndefined();
    for (const address of ["127.0.0.1", "192.0.2.8", "::1", "2001:db8::8"]) {
      expect(loadWorkerConfig({
        ...BASE_ENV,
        ROOST_TERMINAL_PEER_BIND_ADDRESS: address,
      }).terminalPeerBindAddress).toBe(address);
    }
  });

  test("rejects hostnames and non-unicast addresses", () => {
    for (const address of [
      "",
      "worker.example.test",
      "127.0.0.1:3478",
      "0.0.0.0",
      "224.0.0.1",
      "255.255.255.255",
      "::",
      "0:0:0:0:0:0:0:0",
      "ff02::1",
      "::ffff:0.0.0.0",
    ]) {
      expect(() => loadWorkerConfig({
        ...BASE_ENV,
        ROOST_TERMINAL_PEER_BIND_ADDRESS: address,
      })).toThrow(
        "ROOST_TERMINAL_PEER_BIND_ADDRESS must be a literal unicast IPv4 or IPv6 address",
      );
    }
  });
});

describe("ROOST_TERMINAL_PEER_PORT_RANGE", () => {
  test("normalizes bounded inclusive decimal ranges", () => {
    expect(loadWorkerConfig(BASE_ENV).terminalPeerPortRange).toBeUndefined();
    expect(loadWorkerConfig({
      ...BASE_ENV,
      ROOST_TERMINAL_PEER_PORT_RANGE: "1024-65535",
    }).terminalPeerPortRange).toEqual({ min: 1024, max: 65535 });
    expect(loadWorkerConfig({
      ...BASE_ENV,
      ROOST_TERMINAL_PEER_PORT_RANGE: "49152-49153",
    }).terminalPeerPortRange).toEqual({ min: 49152, max: 49153 });
  });

  test("rejects malformed, out-of-bounds, and reversed ranges", () => {
    for (const range of [
      "",
      "1024",
      "1024-",
      "1023-1024",
      "1024-65536",
      "65535-1024",
      "01024-1025",
      "1024 - 1025",
      "+1024-1025",
      "1024.0-1025",
      "1024-1025-1026",
      "1024-1025\n",
      "1024-1025\r",
      "1024-1025\u2028",
      "1024-1025\u2029",
    ]) {
      expect(() => loadWorkerConfig({
        ...BASE_ENV,
        ROOST_TERMINAL_PEER_PORT_RANGE: range,
      })).toThrow(
        "ROOST_TERMINAL_PEER_PORT_RANGE must be min-max with decimal ports from 1024 to 65535",
      );
    }
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
