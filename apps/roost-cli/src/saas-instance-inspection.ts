/**
 * Validates managed-instance configuration and reports activation or public health state.
 * Hidden CLI status and health actions use the same identity checks as bootstrap dispatch.
 * Probing the public listener ensures Caddy reaches the exact configured coordinator instance.
 */

import type { Database } from "bun:sqlite";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import type { CoordConfig } from "@roost/shared/config";
import {
  AuthCoordIdentityRequestSchema,
  AuthCoordIdentityResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import type { AuthCoordIdentityResponse } from "@roost/shared/proto/coordinator_pb";
import { inspectManagedContainerState } from "../../coord/src/managed-container-invariant.ts";
import { normalizedInstanceUuid } from "./saas-instance-command.ts";
import type { OwnerActivationStatus } from "./saas-instance-types.ts";

const MANAGED_PUBLIC_IDENTITY_URL =
  "http://127.0.0.1:4104/roost.v1.CoordinatorService/AuthCoordIdentity";
const HEALTH_TIMEOUT_MS = 5_000;

export function configuredSaasInstanceId(config: CoordConfig): string {
  const instanceId = normalizedInstanceUuid(config.instanceId);
  if (!instanceId) {
    throw new Error("managed instance configuration requires a coordinator UUID");
  }
  return instanceId;
}

export function assertManagedSaasInstance(config: CoordConfig): void {
  if (!config.managedContainer || !config.saasMode) {
    throw new Error("internal SaaS instance actions require managed-container configuration");
  }
}

/** Read the same explicit credential topology that gates managed startup. */
export function readOwnerActivationStatus(
  sqlite: Database,
  coordinatorId: string,
  now: number = Date.now(),
): OwnerActivationStatus {
  const state = inspectManagedContainerState(sqlite, coordinatorId, now);
  return {
    accountId: state.accountId,
    coordinatorId: state.coordinatorId,
    expiresAtMs: state.expiresAtMs,
    activated: state.activated,
    topology: state.topology,
  };
}

/**
 * Probe the container's public listener rather than its private coordinator
 * listener. A healthy process must identify itself as this exact managed
 * instance through the same public edge that Caddy reaches.
 */
export async function checkSaasInstanceHealth(
  config: CoordConfig,
  fetchImpl: typeof globalThis.fetch = globalThis.fetch,
): Promise<void> {
  assertManagedSaasInstance(config);
  const expectedInstanceId = configuredSaasInstanceId(config);
  const request = toBinary(
    AuthCoordIdentityRequestSchema,
    create(AuthCoordIdentityRequestSchema),
  );

  let response: Response;
  try {
    response = await fetchImpl(MANAGED_PUBLIC_IDENTITY_URL, {
      method: "POST",
      headers: {
        "content-type": "application/proto",
        "connect-protocol-version": "1",
      },
      body: request.buffer.slice(
        request.byteOffset,
        request.byteOffset + request.byteLength,
      ) as ArrayBuffer,
      redirect: "error",
      signal: AbortSignal.timeout(HEALTH_TIMEOUT_MS),
    });
  } catch {
    throw new Error("managed coordinator health identity request failed");
  }
  if (!response.ok) {
    throw new Error("managed coordinator health identity request failed");
  }

  let identity: AuthCoordIdentityResponse;
  try {
    identity = fromBinary(
      AuthCoordIdentityResponseSchema,
      new Uint8Array(await response.arrayBuffer()),
    );
  } catch {
    throw new Error("managed coordinator health identity response was invalid");
  }
  if (
    !identity.saasMode
    || !identity.publicListener
    || identity.instanceId !== expectedInstanceId
  ) {
    throw new Error("managed coordinator health identity mismatch");
  }
}
