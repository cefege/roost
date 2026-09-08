import { describe, expect, test } from "bun:test";
import { resolveDeployCoordinatorUrl } from "../src/deploy-jobs.ts";

describe("worker deploy coordinator URL", () => {
  test("prefers the explicit worker target over the declared front doors", () => {
    expect(resolveDeployCoordinatorUrl({
      ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
      ROOST_COORDINATOR_URL: "https://workers.example.test",
      ROOST_COORDINATOR_PUBLIC_URL: "https://coord.example.test",
      ROOST_WEB_PUBLIC_URL: "https://dashboard.example.test",
    })).toBe("https://workers.example.test");
  });

  test("falls back through the coordinator identity origin to the front door", () => {
    expect(resolveDeployCoordinatorUrl({
      ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
      ROOST_COORDINATOR_PUBLIC_URL: "https://coord.example.test",
      ROOST_WEB_PUBLIC_URL: "https://dashboard.example.test",
    })).toBe("https://coord.example.test");
    expect(resolveDeployCoordinatorUrl({
      ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
      ROOST_WEB_PUBLIC_URL: "https://dashboard.example.test",
    })).toBe("https://dashboard.example.test");
  });

  test("treats a declared-but-empty entry as undeclared", () => {
    expect(resolveDeployCoordinatorUrl({
      ROOST_COORDINATOR_PUBLIC_URL: "",
      ROOST_WEB_PUBLIC_URL: "https://dashboard.example.test",
    })).toBe("https://dashboard.example.test");
  });

  test("refuses when no front door is declared", () => {
    expect(resolveDeployCoordinatorUrl({
      ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
      ROOST_REACHABLE_ADDR: "coord.tailnet.ts.net",
    })).toBeNull();
  });

  test("refuses an origin a remote worker cannot dial", () => {
    for (const url of [
      "http://127.0.0.1:4103",
      "http://localhost:4103",
      "https://mac-mini.local:4102",
      "not-a-url",
    ]) {
      expect(resolveDeployCoordinatorUrl({ ROOST_COORDINATOR_URL: url })).toBeNull();
    }
  });
});
