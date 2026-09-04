// These tests pin the handoff from the managed profile to every managed E2E scenario.
// Resource references fail closed, while generated verification keys must pass the
// coordinator's real OpenSSH importer and Ed25519 signature verification path.

import { expect, test } from "bun:test";
import { sign } from "node:crypto";
import { mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  FEDERATED_ASSERTION_AUDIENCE,
  FEDERATED_ASSERTION_ISSUER,
  GOOGLE_IDENTITY_ISSUER,
  createFederatedAssertionVerifier,
} from "../../coord/src/connect/federated-assertion.ts";
import {
  requiredManagedE2eResources,
  writeEd25519VerificationKeyFixture,
} from "./managed-e2e-fixture.ts";

const IMAGE_ID = `sha256:${"a".repeat(64)}`;
const NETWORK = "roost-managed-e2e-1234-deadbeef";
const GIT_SHA = "b".repeat(40);

test("writes a 0644 key accepted by the coordinator federated assertion verifier", async () => {
  const root = mkdtempSync(join(tmpdir(), "roost-managed-key-fixture-"));
  try {
    const verifyKeyPath = join(root, "saas-auth-verify-key");
    const privateKey = writeEd25519VerificationKeyFixture(verifyKeyPath);
    expect(statSync(verifyKeyPath).mode & 0o777).toBe(0o644);

    const nowMs = 1_800_000_000_000;
    const nowSeconds = Math.floor(nowMs / 1_000);
    const claims = {
      iss: FEDERATED_ASSERTION_ISSUER,
      aud: FEDERATED_ASSERTION_AUDIENCE,
      purpose: "continue",
      account_id: "11111111-1111-4111-8111-111111111111",
      coordinator_id: "22222222-2222-4222-8222-222222222222",
      route_key: "c".repeat(64),
      identity_issuer: GOOGLE_IDENTITY_ISSUER,
      identity_subject: "managed-key-fixture",
      email_normalized: "managed-key-fixture@example.com",
      device_fp: "d".repeat(64),
      jti: "33333333-3333-4333-8333-333333333333",
      iat: nowSeconds,
      exp: nowSeconds + 60,
    } as const;
    const header = Buffer.from(JSON.stringify({ alg: "EdDSA", typ: "JWT" })).toString("base64url");
    const payload = Buffer.from(JSON.stringify(claims)).toString("base64url");
    const signingInput = `${header}.${payload}`;
    const signature = sign(null, Buffer.from(signingInput), privateKey).toString("base64url");
    const verifier = createFederatedAssertionVerifier(verifyKeyPath);

    await expect(verifier(`${signingInput}.${signature}`, "continue", nowMs)).resolves.toEqual(claims);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("returns the exact profile-owned image and shared network", () => {
  expect(requiredManagedE2eResources({
    ROOST_SAAS_E2E_IMAGE: IMAGE_ID,
    ROOST_SAAS_E2E_NETWORK: NETWORK,
    ROOST_SAAS_E2E_GIT_SHA: GIT_SHA,
  })).toEqual({ imageId: IMAGE_ID, network: NETWORK, gitSha: GIT_SHA });
});

test("has no image fallback", () => {
  expect(() => requiredManagedE2eResources({
    ROOST_SAAS_E2E_NETWORK: NETWORK,
  })).toThrow("ROOST_SAAS_E2E_IMAGE");
});

test("has no network fallback", () => {
  expect(() => requiredManagedE2eResources({
    ROOST_SAAS_E2E_IMAGE: IMAGE_ID,
  })).toThrow("ROOST_SAAS_E2E_NETWORK");
});

test("rejects a mutable image reference and malformed network", () => {
  expect(() => requiredManagedE2eResources({
    ROOST_SAAS_E2E_IMAGE: "roost-coord:latest",
    ROOST_SAAS_E2E_NETWORK: NETWORK,
  })).toThrow("immutable sha256 image ID");
  expect(() => requiredManagedE2eResources({
    ROOST_SAAS_E2E_IMAGE: IMAGE_ID,
    ROOST_SAAS_E2E_NETWORK: " network with spaces ",
  })).toThrow("Docker network name");
});

test("requires an exact 40-hex profile SHA", () => {
  expect(() => requiredManagedE2eResources({
    ROOST_SAAS_E2E_IMAGE: IMAGE_ID,
    ROOST_SAAS_E2E_NETWORK: NETWORK,
  })).toThrow("ROOST_SAAS_E2E_GIT_SHA");
  expect(() => requiredManagedE2eResources({
    ROOST_SAAS_E2E_IMAGE: IMAGE_ID,
    ROOST_SAAS_E2E_NETWORK: NETWORK,
    ROOST_SAAS_E2E_GIT_SHA: "b".repeat(39),
  })).toThrow("exactly 40 hexadecimal characters");
});
