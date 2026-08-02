import { describe, expect, test } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  AccessLayerAuthError,
  classifyAuthFailure,
} from "../src/connect.ts";

const WORKERS_LIST = "/roost.v1.CoordinatorService/WorkersList";

describe("browser auth failure classification", () => {
  test("Access-layer failures remain nondestructive", () => {
    expect(classifyAuthFailure(new AccessLayerAuthError(), WORKERS_LIST)).toBe("access");
    expect(classifyAuthFailure(
      new ConnectError("wrapped", Code.Unavailable, undefined, undefined, new AccessLayerAuthError()),
      WORKERS_LIST,
    )).toBe("access");
  });

  test("only a marked device rejection from a known auth-required RPC is authoritative", () => {
    const marked = new ConnectError(
      "authentication required",
      Code.Unauthenticated,
      new Headers({ "x-roost-auth-layer": "device" }),
    );
    expect(classifyAuthFailure(marked, WORKERS_LIST)).toBe("device");
    expect(classifyAuthFailure(marked, "/roost.v1.CoordinatorService/AuthCoordIdentity"))
      .toBe("retryable");
  });

  test("unmarked 401s, network errors, and 5xx-shaped failures are retryable", () => {
    expect(classifyAuthFailure(
      new ConnectError("unauthenticated", Code.Unauthenticated),
      WORKERS_LIST,
    )).toBe("retryable");
    expect(classifyAuthFailure(new TypeError("network failed"), WORKERS_LIST)).toBe("retryable");
    expect(classifyAuthFailure(
      new ConnectError("upstream", Code.Unavailable),
      WORKERS_LIST,
    )).toBe("retryable");
  });
});
