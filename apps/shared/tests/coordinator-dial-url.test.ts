// Pins the one precedence rule for the coordinator origin a worker dials.
// Coord's deploy composer and roost-cli enrollment both depend on this order and on
// the refusal, so a swapped variable or a revived derived default fails here.

import { describe, expect, test } from "bun:test";
import {
  COORDINATOR_DIAL_URL_REQUIRED_MESSAGE,
  resolveCoordinatorDialUrl,
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
