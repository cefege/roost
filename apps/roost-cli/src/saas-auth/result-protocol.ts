/**
 * Delivers provisioning outcomes and binds ready assertions to one device fingerprint.
 * Gateway result routes call this protocol with durable receipts and a federation signer.
 * Browser-bound cookies and one-time state prevent polling or assertions leaking across clients.
 */

import { randomUUID } from "node:crypto";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { canonicalJson, parseStrictJson } from "./canonical-json.ts";
import type { FederatedAssertionSigner } from "./federated-assertion.ts";
import { RESULT_COOKIE } from "./google-oauth.ts";
import { parseCentralAssertionInputs, type ProvisionerClient, type ProvisionerStatus } from "./provisioner-client.ts";
import {
  exactObject,
  gatewayJson,
  InvalidGatewayRequest,
  requestCookie,
  boundedText,
} from "./request-security.ts";
import { GatewayStateError, type GatewayResultReceipt, type GatewayStateStore } from "./state-store.ts";

export interface ResultProtocolOptions {
  store: GatewayStateStore;
  provisioner: ProvisionerClient;
  signer: FederatedAssertionSigner;
  now?: () => number;
}

function decodeEd25519PublicKey(value: unknown): Uint8Array {
  const encoded = boundedText(value, 1_024);
  if (!/^[A-Za-z0-9+/]{42}[AEIMQUYcgkosw048]=$/u.test(encoded)) throw new InvalidGatewayRequest();
  const raw = Buffer.from(encoded, "base64");
  if (raw.byteLength !== 32 || raw.toString("base64") !== encoded) throw new InvalidGatewayRequest();
  return new Uint8Array(raw);
}

export class ResultProtocol {
  private readonly now: () => number;

  constructor(private readonly options: ResultProtocolOptions) {
    this.now = options.now ?? Date.now;
  }

  private receipt(request: Request): string | null {
    return requestCookie(request, RESULT_COOKIE);
  }

  private publicStored(result: GatewayResultReceipt): Response {
    if (result.state === "pending") return gatewayJson({ state: "pending" }, 202, { "retry-after": "1" });
    if (result.state === "awaiting-device") return gatewayJson({ state: "awaiting-device", routeKey: result.routeKey }, 200);
    if (result.state === "ready") return gatewayJson({ state: "ready", routeKey: result.routeKey, assertion: result.assertion }, 200);
    return gatewayJson({ state: result.state }, 200);
  }

  private async persistWorkerStatus(receipt: string, result: GatewayResultReceipt, status: ProvisionerStatus): Promise<Response> {
    if (status.state === "pending") {
      return gatewayJson({ state: "pending" }, 202, {
        "retry-after": String(Math.max(1, Math.ceil(status.retryAfterMs / 1_000))),
      });
    }
    if (status.state === "failed") {
      this.options.store.setResultOutcome({ jobId: result.jobId, state: "failed", nowMs: this.now() });
      return gatewayJson({ state: "failed" }, 200);
    }
    if (!("assertionInputs" in status)) {
      return gatewayJson({ state: "ready", routeKey: status.routeKey }, 200);
    }
    const serialized = canonicalJson(status.assertionInputs);
    if (!this.options.store.setResultOutcome({
      jobId: result.jobId,
      state: "awaiting-device",
      routeKey: status.routeKey,
      assertionInput: serialized,
      nowMs: this.now(),
    })) return gatewayJson({ error: "request unavailable" }, 503);
    if (status.state === "awaiting-device") {
      return gatewayJson({ state: "awaiting-device", routeKey: status.routeKey }, 200);
    }
    const fingerprint = status.assertionInputs.deviceFingerprint;
    if (!fingerprint) return gatewayJson({ error: "request unavailable" }, 503);
    const assertion = await this.options.signer.sign(status.assertionInputs, fingerprint);
    const stored = this.options.store.bindResultAssertion(receipt, fingerprint, assertion, this.now());
    if (!stored) return gatewayJson({ error: "request unavailable" }, 503);
    return gatewayJson({ state: "ready", routeKey: status.routeKey, assertion: stored }, 200);
  }

  async get(request: Request): Promise<Response> {
    const receipt = this.receipt(request);
    if (!receipt) return gatewayJson({ state: "failed" }, 200);
    const result = this.options.store.getResult(receipt, this.now());
    if (!result) return gatewayJson({ state: "failed" }, 200);
    if (result.state !== "pending") return this.publicStored(result);
    try {
      return await this.persistWorkerStatus(receipt, result, await this.options.provisioner.status(result.jobId));
    } catch {
      return gatewayJson({ state: "pending" }, 202, { "retry-after": "1" });
    }
  }

  async bind(request: Request, body: unknown): Promise<Response> {
    const receipt = this.receipt(request);
    if (!receipt) return gatewayJson({ error: "request rejected" }, 403);
    const input = exactObject(body, ["sshPubkeyB64"]);
    const fingerprint = await fingerprintOf(decodeEd25519PublicKey(input.sshPubkeyB64));
    const result = this.options.store.getResult(receipt, this.now());
    if (!result) return gatewayJson({ error: "request rejected" }, 403);
    if (result.state === "ready") {
      if (result.boundFingerprint !== fingerprint || !result.assertion || !result.routeKey) {
        return gatewayJson({ error: "request rejected" }, 409);
      }
      return gatewayJson({ state: "ready", routeKey: result.routeKey, assertion: result.assertion }, 200);
    }
    if (result.state !== "awaiting-device" || !result.routeKey) return gatewayJson({ error: "request rejected" }, 409);
    const serialized = this.options.store.getAssertionInput(receipt, this.now());
    if (!serialized) return gatewayJson({ error: "request rejected" }, 409);
    const inputs = parseCentralAssertionInputs(parseStrictJson(serialized));
    if (inputs.purpose !== "continue" || inputs.routeKey !== result.routeKey) return gatewayJson({ error: "request rejected" }, 409);
    const assertion = await this.options.signer.sign(inputs, fingerprint);
    try {
      const stored = this.options.store.bindResultAssertion(receipt, fingerprint, assertion, this.now());
      if (!stored) return gatewayJson({ error: "request rejected" }, 409);
      return gatewayJson({ state: "ready", routeKey: result.routeKey, assertion: stored }, 200);
    } catch (error) {
      if (error instanceof GatewayStateError && error.code === "conflict") return gatewayJson({ error: "request rejected" }, 409);
      throw error;
    }
  }

  async completeLink(request: Request, body: unknown): Promise<Response> {
    exactObject(body, []);
    const receipt = this.receipt(request);
    if (!receipt) return gatewayJson({ error: "request rejected" }, 403);
    const result = this.options.store.getResult(receipt, this.now());
    if (!result || result.state !== "ready") return gatewayJson({ error: "request rejected" }, 409);
    const serialized = this.options.store.getAssertionInput(receipt, this.now());
    if (!serialized || parseCentralAssertionInputs(parseStrictJson(serialized)).purpose !== "link") {
      return gatewayJson({ error: "request rejected" }, 409);
    }
    try {
      const status = await this.options.provisioner.finalizeLink(result.jobId);
      return status.state === "ready"
        ? gatewayJson({ state: "ready" }, 200)
        : gatewayJson({ state: "pending" }, 202, { "retry-after": "1" });
    } catch {
      return gatewayJson({ state: "pending" }, 202, { "retry-after": "1" });
    }
  }
}
