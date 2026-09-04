/**
 * Dispatches hidden managed-instance bootstrap, activation, status, and health commands.
 * The public CLI imports this entry point lazily so normal commands avoid coordinator startup code.
 * Focused modules own privileged parsing and transactions while this file preserves the existing API.
 */

import { loadCoordConfig } from "@roost/shared/config";
import { loadOrCreateCoordKey } from "../../coord/src/coord-key.ts";
import { openDb } from "../../coord/src/db/connection.ts";
import { runMigrations } from "../../coord/src/db/migrate.ts";
import { MIGRATIONS } from "../../coord/src/migrations-embed.generated.ts";
import {
  parseGoogleOwnerSeedPayload,
  parseSaasInstanceCommand,
  parseSignupGatewayActivationHash,
  readSaasInstanceSeedStdin,
} from "./saas-instance-command.ts";
import { seedGoogleOwner } from "./saas-instance-google-owner.ts";
import {
  assertManagedSaasInstance,
  checkSaasInstanceHealth,
  configuredSaasInstanceId,
  readOwnerActivationStatus,
} from "./saas-instance-inspection.ts";
import {
  releaseOwnerActivationEmail,
  seedOwnerActivation,
  seedSignupGatewayOwnerActivation,
} from "./saas-instance-owner-activation.ts";
import type {
  OwnerActivationIdentity,
  OwnerActivationStatus,
  SaasInstanceCommand,
  SeedGoogleOwnerOptions,
  SeedGoogleOwnerPayload,
  SeedOwnerActivationInput,
  SeedOwnerActivationOptions,
  SeedOwnerActivationResult,
  SeedOwnerIdentityInput,
  SeedSignupGatewayOwnerActivationOptions,
} from "./saas-instance-types.ts";

export {
  checkSaasInstanceHealth,
  parseGoogleOwnerSeedPayload,
  parseSaasInstanceCommand,
  parseSignupGatewayActivationHash,
  readOwnerActivationStatus,
  releaseOwnerActivationEmail,
  seedGoogleOwner,
  seedOwnerActivation,
  seedSignupGatewayOwnerActivation,
};
export type {
  OwnerActivationIdentity,
  OwnerActivationStatus,
  SaasInstanceCommand,
  SeedGoogleOwnerOptions,
  SeedGoogleOwnerPayload,
  SeedOwnerActivationInput,
  SeedOwnerActivationOptions,
  SeedOwnerActivationResult,
  SeedOwnerIdentityInput,
  SeedSignupGatewayOwnerActivationOptions,
};

async function withoutInformationalStdout<T>(operation: () => Promise<T>): Promise<T> {
  const original = console.log;
  console.log = () => {};
  try {
    return await operation();
  } finally {
    console.log = original;
  }
}

/** Hidden compiled-binary entry point. The module is imported lazily by main so
 * ordinary CLI commands do not pull coordinator/database code into startup. */
export async function saasInstance(args: string[]): Promise<void> {
  const command = parseSaasInstanceCommand(args);
  const config = loadCoordConfig(process.env as Record<string, string | undefined>);
  assertManagedSaasInstance(config);
  const expectedInstanceId = configuredSaasInstanceId(config);
  if (command.action === "health") {
    await checkSaasInstanceHealth(config);
    console.log(JSON.stringify({
      event: "saas_instance.health",
      status: "ok",
    }));
    return;
  }
  if (
    "input" in command
    && command.input.coordinatorId !== expectedInstanceId
  ) {
    throw new Error("seed coordinator ID does not match managed instance configuration");
  }
  let stdinSeed: string | SeedGoogleOwnerPayload | undefined;
  if (command.action === "seed-signup-gateway-owner-activation") {
    stdinSeed = parseSignupGatewayActivationHash(await readSaasInstanceSeedStdin());
  } else if (command.action === "seed-google-owner") {
    stdinSeed = parseGoogleOwnerSeedPayload(await readSaasInstanceSeedStdin());
  }

  const opened = openDb(config.dbPath, { managedContainer: true });
  let output: Record<string, unknown>;
  let operationFailed = false;
  try {
    await withoutInformationalStdout(() =>
      runMigrations(opened.sqlite, MIGRATIONS.length > 0 ? MIGRATIONS : undefined)
    );
    if (
      command.action === "seed-owner-activation"
      || command.action === "seed-signup-gateway-owner-activation"
      || command.action === "seed-google-owner"
    ) {
      await withoutInformationalStdout(() => loadOrCreateCoordKey(config.coordKeyPath));
    }
    if (command.action === "seed-owner-activation") {
      if (!config.emailOutboxKey || !config.webPublicUrl || !config.tenantRouteKey) {
        throw new Error("managed owner activation requires email, public URL, and tenant route configuration");
      }
      const result = seedOwnerActivation(opened.sqlite, command.input, {
        emailOutboxKey: config.emailOutboxKey,
        webPublicUrl: config.webPublicUrl,
        tenantRouteKey: config.tenantRouteKey,
      });
      output = {
        event: "saas_instance.owner_activation_seeded",
        status: "held",
        account_id: result.accountId,
        coordinator_id: result.coordinatorId,
      };
    } else if (command.action === "seed-signup-gateway-owner-activation") {
      const result = seedSignupGatewayOwnerActivation(
        opened.sqlite,
        command.input,
        stdinSeed as string,
      );
      output = {
        event: "saas_instance.signup_gateway_owner_activation_seeded",
        status: "held",
        account_id: result.accountId,
        coordinator_id: result.coordinatorId,
      };
    } else if (command.action === "seed-google-owner") {
      const result = seedGoogleOwner(
        opened.sqlite,
        command.input,
        stdinSeed as SeedGoogleOwnerPayload,
      );
      output = {
        event: "saas_instance.google_owner_seeded",
        status: "active",
        account_id: result.accountId,
        coordinator_id: result.coordinatorId,
      };
    } else if (command.action === "release-owner-activation-email") {
      const result = releaseOwnerActivationEmail(opened.sqlite, Date.now, expectedInstanceId);
      output = {
        event: "saas_instance.owner_activation_email_released",
        status: "due",
        account_id: result.accountId,
        coordinator_id: result.coordinatorId,
      };
    } else {
      const result = readOwnerActivationStatus(opened.sqlite, expectedInstanceId);
      output = {
        event: "saas_instance.activation_status",
        activated: result.activated,
        credential_topology: result.topology,
        expires_at_ms: result.expiresAtMs,
        account_id: result.accountId,
        coordinator_id: result.coordinatorId,
      };
    }
  } catch (error) {
    operationFailed = true;
    throw error;
  } finally {
    try {
      await opened.close();
    } catch (error) {
      if (!operationFailed) throw error;
    }
  }
  console.log(JSON.stringify(output));
}
