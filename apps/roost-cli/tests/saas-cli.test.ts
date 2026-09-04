import { describe, expect, test } from "bun:test";
import { parseSaasCommand } from "../src/saas/index.ts";
import {
  _assertSaasEntryAdmission,
  type SaasEntryIdentity,
} from "../src/saas/entry-admission.ts";

const DIGEST = `sha256:${"a".repeat(64)}`;

const ROOT_IDENTITY: SaasEntryIdentity = {
  platform: "linux",
  username: "root",
  uid: 0,
  effectiveUid: 0,
  gid: 0,
  effectiveGid: 0,
  managedContainer: undefined,
  saasMode: undefined,
  trustProxy: undefined,
  coordinatorBind: undefined,
  publicBind: undefined,
};
const AUTH_IDENTITY: SaasEntryIdentity = {
  ...ROOT_IDENTITY,
  username: "roost-signup",
  uid: 991,
  effectiveUid: 991,
  gid: 991,
  effectiveGid: 991,
};
const INSTANCE_IDENTITY: SaasEntryIdentity = {
  ...ROOT_IDENTITY,
  username: "nonroot",
  uid: 65_532,
  effectiveUid: 65_532,
  gid: 65_532,
  effectiveGid: 65_532,
  managedContainer: "1",
  saasMode: "1",
  trustProxy: "1",
  coordinatorBind: "127.0.0.1:4103",
  publicBind: "0.0.0.0:4104",
};

describe("SaaS CLI parsing", () => {
  test("accepts only the documented non-secret operator arguments", () => {
    expect(parseSaasCommand(["account-create", "--email", "owner@example.com"]))
      .toEqual({ action: "account-create", email: "owner@example.com" });
    expect(parseSaasCommand(["account-resend", "--email", "owner@example.com"]))
      .toEqual({ action: "account-resend", email: "owner@example.com" });
    expect(parseSaasCommand(["account-disable", "--email", "owner@example.com", "--yes"]))
      .toEqual({ action: "account-disable", email: "owner@example.com" });
    expect(parseSaasCommand(["account-enable", "--email", "owner@example.com"]))
      .toEqual({ action: "account-enable", email: "owner@example.com" });
    expect(parseSaasCommand(["accounts"])).toEqual({ action: "accounts" });
    expect(parseSaasCommand(["backup"])).toEqual({ action: "backup" });
    expect(parseSaasCommand(["backup", "--email", "owner@example.com"]))
      .toEqual({ action: "backup", email: "owner@example.com" });
    expect(parseSaasCommand(["rollout", "--image", DIGEST]))
      .toEqual({ action: "rollout", imageDigest: DIGEST });
    expect(parseSaasCommand(["reconcile"])).toEqual({ action: "reconcile" });
    expect(parseSaasCommand(["resolver"])).toEqual({ action: "resolver" });
  });

  test("rejects passwords, hostnames, tags, roles, confirmation omissions, and unknown flags", () => {
    for (const args of [
      ["account-create", "--email", "owner@example.com", "--password", "secret"],
      ["account-create", "--hostname", "chosen.example"],
      ["account-create", "--image", "roost:latest"],
      ["account-create", "--role", "admin"],
      ["account-disable", "--email", "owner@example.com"],
      ["account-disable", "--yes", "--email", "owner@example.com"],
      ["rollout", "--image", DIGEST, "--parallel"],
      ["backup", "--unknown"],
      ["reconcile", "--force"],
      ["resolver", "--hmac-key", "secret"],
      ["resolver", "--bind", "0.0.0.0:4107"],
    ]) {
      expect(() => parseSaasCommand(args)).toThrow("usage: roost saas");
    }
  });
});

describe("SaaS dispatch admission", () => {
  test("admits each production identity only for its external dispatch profile", () => {
    expect(() => _assertSaasEntryAdmission("operator", ROOT_IDENTITY)).not.toThrow();
    expect(() => _assertSaasEntryAdmission("provisioner", ROOT_IDENTITY)).not.toThrow();
    expect(() => _assertSaasEntryAdmission("auth", AUTH_IDENTITY)).not.toThrow();
    expect(() => _assertSaasEntryAdmission("instance", INSTANCE_IDENTITY)).not.toThrow();

    expect(() => _assertSaasEntryAdmission("operator", AUTH_IDENTITY))
      .toThrow("SaaS operator entry is not admitted");
    expect(() => _assertSaasEntryAdmission("auth", ROOT_IDENTITY))
      .toThrow("SaaS auth entry is not admitted");
    expect(() => _assertSaasEntryAdmission("instance", AUTH_IDENTITY))
      .toThrow("SaaS instance entry is not admitted");
    expect(() => _assertSaasEntryAdmission("provisioner", INSTANCE_IDENTITY))
      .toThrow("SaaS provisioner entry is not admitted");
  });
});
