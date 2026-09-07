// Deploy identity resolution contract: a remote target's ROOST_WORKER_LABEL /
// ROOST_REACHABLE_ADDR may come only from a `roost deploy` flag or from that
// target's own installed service definition.
//
// Regression guard for a live misidentification: deploying to a fresh Mac from
// a box whose shell exported ROOST_WORKER_LABEL=ovh1-8c32g installed the Mac
// under that label, so the coordinator listed two ovh1-8c32g workers and the
// Mac's real identity disappeared from the fleet view.
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { DeployFailure } from "../src/deploy-exec.ts";
import {
  _resolveDeployEnvValue,
  resolveRemoteDeployIdentityEnv,
} from "../src/deploy-plist-env.ts";

const KEYS = ["ROOST_COORDINATOR_URL", "ROOST_REACHABLE_ADDR", "ROOST_WORKER_LABEL"] as const;

const TARGET = "mihai-m1-old.tail67850e.ts.net";
const INSTALLED_IDENTITY = {
  ROOST_WORKER_LABEL: "mike-m1-air-old",
  ROOST_REACHABLE_ADDR: "mike-m1-air-old.tail67850e.ts.net",
};

let saved: Record<string, string | undefined> = {};

/** The deploying box's own exported identity — the value that must never
 *  reach a remote target's install. */
function exportDeployingBoxIdentity(): void {
  process.env.ROOST_WORKER_LABEL = "ovh1-8c32g";
  process.env.ROOST_REACHABLE_ADDR = "ovh1-8c32g.tail67850e.ts.net";
}

function identityFailure(call: () => unknown): DeployFailure {
  try {
    call();
  } catch (error) {
    if (error instanceof DeployFailure) return error;
    throw error;
  }
  throw new Error("expected the deploy to refuse an ambiguous target identity");
}

beforeEach(() => {
  saved = {};
  for (const key of KEYS) {
    saved[key] = process.env[key];
    delete process.env[key];
  }
});

afterEach(() => {
  for (const key of KEYS) {
    if (saved[key] === undefined) delete process.env[key];
    else process.env[key] = saved[key];
  }
});

describe("remote deploy identity", () => {
  test("a fresh remote target never adopts the deploying box's identity", () => {
    exportDeployingBoxIdentity();
    expect(_resolveDeployEnvValue("ROOST_WORKER_LABEL", {}, undefined, "remote")).toBeUndefined();
    expect(_resolveDeployEnvValue("ROOST_REACHABLE_ADDR", {}, undefined, "remote")).toBeUndefined();
  });

  test("an ambient identity with nothing to resolve from refuses the deploy", () => {
    exportDeployingBoxIdentity();
    const failure = identityFailure(() => resolveRemoteDeployIdentityEnv(TARGET, {}));
    expect(failure.exitCode).toBe(6);
    expect(failure.message).toContain("ROOST_WORKER_LABEL");
    expect(failure.message).toContain(TARGET);
    expect(failure.message).toContain("--label=<value>");
    expect(failure.message).not.toContain("ovh1-8c32g");
  });

  test("an ambient reachable address alone still names its own flag", () => {
    process.env.ROOST_REACHABLE_ADDR = "ovh1-8c32g.tail67850e.ts.net";
    const failure = identityFailure(() => resolveRemoteDeployIdentityEnv(TARGET, {}));
    expect(failure.message).toContain("ROOST_REACHABLE_ADDR");
    expect(failure.message).toContain("--reachable-addr=<value>");
  });

  test("invocation flags supply the target's identity", () => {
    exportDeployingBoxIdentity();
    expect(resolveRemoteDeployIdentityEnv(TARGET, {}, {
      workerLabel: "mike-m1-air-old",
      reachableAddr: "mike-m1-air-old.tail67850e.ts.net",
    })).toEqual(INSTALLED_IDENTITY);
  });

  test("the target's installed identity wins over an ambient one", () => {
    exportDeployingBoxIdentity();
    expect(resolveRemoteDeployIdentityEnv(TARGET, { ...INSTALLED_IDENTITY }))
      .toEqual(INSTALLED_IDENTITY);
  });

  test("an unset ambient identity leaves the target to derive its own", () => {
    expect(resolveRemoteDeployIdentityEnv(TARGET, {})).toEqual({
      ROOST_WORKER_LABEL: undefined,
      ROOST_REACHABLE_ADDR: undefined,
    });
  });

  test("fleet-wide keys keep their ambient fallback", () => {
    process.env.ROOST_COORDINATOR_URL = "https://coord.tail67850e.ts.net:4102";
    expect(_resolveDeployEnvValue("ROOST_COORDINATOR_URL", {}, undefined, "remote"))
      .toBe("https://coord.tail67850e.ts.net:4102");
  });

  test("an inherited Object key is not an identity key", () => {
    Reflect.set(process.env, "constructor", "ambient");
    try {
      expect(_resolveDeployEnvValue("constructor", {}, undefined, "remote")).toBe("ambient");
    } finally {
      Reflect.deleteProperty(process.env, "constructor");
    }
  });
});

describe("self deploy identity", () => {
  test("the ambient identity is the target's own identity", () => {
    exportDeployingBoxIdentity();
    expect(_resolveDeployEnvValue("ROOST_WORKER_LABEL", {}, undefined, "self"))
      .toBe("ovh1-8c32g");
    expect(_resolveDeployEnvValue("ROOST_REACHABLE_ADDR", {}, undefined, "self"))
      .toBe("ovh1-8c32g.tail67850e.ts.net");
  });
});
