import { describe, expect, test } from "bun:test";
import { resolveDeployCoordinatorUrl } from "../src/deploy-jobs.ts";

describe("worker deploy coordinator URL", () => {
  test("uses the public URL ahead of the private loopback bind", () => {
    expect(resolveDeployCoordinatorUrl({
      ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
      ROOST_COORDINATOR_PUBLIC_URL: "https://coord.tailnet.ts.net:4102",
    }, "coord.tailnet.ts.net")).toBe("https://coord.tailnet.ts.net:4102");
  });

  test("derives the advertised Tailscale port instead of the loopback port", () => {
    expect(resolveDeployCoordinatorUrl({
      ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
      ROOST_REACHABLE_ADDR: "coord.tailnet.ts.net",
    }, "coord.tailnet.ts.net")).toBe("https://coord.tailnet.ts.net:4102");
  });

  test("honors an explicit advertised Tailscale port", () => {
    expect(resolveDeployCoordinatorUrl({
      ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
      ROOST_TAILNET_HTTPS_PORT: "8443",
    }, "coord.tailnet.ts.net")).toBe("https://coord.tailnet.ts.net:8443");
  });
});
