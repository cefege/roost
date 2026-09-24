// Pins the one precedence rule for the coordinator origin a worker dials.
// Coord's deploy composer and roost-cli enrollment both depend on this order and on
// the refusal, so a swapped variable or a revived derived default fails here.

import { describe, expect, test } from "bun:test";
import {
  COORDINATOR_DIAL_URL_REQUIRED_MESSAGE,
  resolveCoordinatorDialUrl,
  workerCoordinatorUrl,
} from "../src/coordinator-dial-url.ts";

describe("coordinator dial URL", () => {
  test("prefers the explicit worker target, then coordinator identity, then front door", () => {
    expect(resolveCoordinatorDialUrl({
      ROOST_COORDINATOR_URL: "https://worker-target.example",
      ROOST_COORDINATOR_PUBLIC_URL: "https://identity.example",
      ROOST_WEB_PUBLIC_URL: "https://front-door.example",
    })).toBe("https://worker-target.example");

    expect(resolveCoordinatorDialUrl({
      ROOST_COORDINATOR_PUBLIC_URL: "https://identity.example",
      ROOST_WEB_PUBLIC_URL: "https://front-door.example",
    })).toBe("https://identity.example");

    expect(resolveCoordinatorDialUrl({
      ROOST_WEB_PUBLIC_URL: "https://front-door.example",
    })).toBe("https://front-door.example");
  });

  test("treats a declared-but-empty entry as undeclared", () => {
    expect(resolveCoordinatorDialUrl({
      ROOST_COORDINATOR_PUBLIC_URL: "",
      ROOST_WEB_PUBLIC_URL: "  https://front-door.example  ",
    })).toBe("https://front-door.example");
  });

  test("refuses instead of deriving an origin", () => {
    expect(resolveCoordinatorDialUrl({})).toBeNull();
    expect(resolveCoordinatorDialUrl({ ROOST_REACHABLE_ADDR: "host.tail1234.ts.net" }))
      .toBeNull();
    expect(COORDINATOR_DIAL_URL_REQUIRED_MESSAGE).toContain("ROOST_COORDINATOR_URL");
    expect(COORDINATOR_DIAL_URL_REQUIRED_MESSAGE).toContain("ROOST_COORDINATOR_PUBLIC_URL");
    expect(COORDINATOR_DIAL_URL_REQUIRED_MESSAGE).toContain("ROOST_WEB_PUBLIC_URL");
  });
});

describe("worker coordinator URL", () => {
  const activeOrigin = "https://roost.example.com";

  test("a declared coordinator origin outranks the active coordinator's own origin", () => {
    expect(workerCoordinatorUrl("https://private.example.ts.net:4102", activeOrigin))
      .toBe("https://private.example.ts.net:4102");
  });

  test("the active origin is the answer only when nothing is declared", () => {
    for (const declared of [undefined, "", "   "]) {
      expect(workerCoordinatorUrl(declared, activeOrigin)).toBe(activeOrigin);
    }
    expect(workerCoordinatorUrl(activeOrigin, activeOrigin)).toBe(activeOrigin);
  });

  test("an invalid declared origin refuses instead of masking itself", () => {
    for (const declared of [
      "http://private.example.ts.net:4102",
      "https://localhost:4102",
      "https://subdomain.localhost:4102",
      "https://127.0.0.1:4102",
      "https://127.0.0.2:4102",
      "https://[::1]:4102",
      "https://private.example.ts.net:4102/?token=bad",
      "https://private.example.ts.net:4102/#fragment",
      "https://private.example.ts.net:4102/path",
      "https://user:pw@private.example.ts.net:4102",
      "not a url",
    ]) {
      expect(workerCoordinatorUrl(declared, activeOrigin)).toBeNull();
    }
  });

  test("normalizes accepted HTTPS root origins", () => {
    expect(workerCoordinatorUrl("https://private.example.ts.net:4102/", activeOrigin))
      .toBe("https://private.example.ts.net:4102");
    expect(workerCoordinatorUrl("https://private.example.ts.net:443/", activeOrigin))
      .toBe("https://private.example.ts.net");
  });

  test("an invalid active origin refuses rather than guessing", () => {
    for (const active of [
      "http://localhost:5173",
      "https://127.0.0.1:4103",
      "https://[::1]:4103",
      "not a url",
    ]) {
      expect(workerCoordinatorUrl(undefined, active)).toBeNull();
    }
  });
});
