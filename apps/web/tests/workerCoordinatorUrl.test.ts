// Covers the Add machine dial-URL contract: a declared coordinator origin is
// authoritative, the active coordinator's own origin is the fallback, and an
// unusable value refuses rather than substituting a different door.

import { describe, expect, test } from "bun:test";
import { workerCoordinatorUrl } from "../src/lib/workerCoordinatorUrl.ts";

describe("worker coordinator origin", () => {
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

  test("a declared origin that cannot be dialed refuses instead of masking itself", () => {
    for (const declared of [
      "http://private.example.ts.net:4102",
      "https://localhost:4102",
      "https://127.0.0.1:4102",
      "https://private.example.ts.net:4102/?token=bad",
      "https://private.example.ts.net:4102/path",
      "https://user:pw@private.example.ts.net:4102",
      "not a url",
    ]) {
      expect(workerCoordinatorUrl(declared, activeOrigin)).toBeNull();
    }
  });

  test("no reachable origin at all refuses rather than guessing", () => {
    for (const active of ["http://localhost:5173", "https://127.0.0.1:4103", "not a url"]) {
      expect(workerCoordinatorUrl(undefined, active)).toBeNull();
    }
  });
});
