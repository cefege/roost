// Implements the operator-facing SaaS CLI commands and composes their dependencies.
// The root CLI dispatches account, reconcile, backup, and rollout actions here.
// Shared construction keeps privileged host checks ahead of tenant side effects.
import { MANAGED_WEB_PUBLIC_ORIGIN } from "@roost/shared/config";
import { existsSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { createEncryptedBackup } from "./backup.ts";
import { CaddyTenantRouter } from "./caddy.ts";
import { ManagedInstanceRuntime } from "./docker.ts";
import { HostAdmission, assertSaasHostPrerequisites, loadSaasHostConfig } from "./host.ts";
import { SaasLifecycle, type TenantRouteManager } from "./lifecycle.ts";
import { ManagedRouteProbe } from "./probe.ts";
import { SaasRollout } from "./rollout.ts";
import { SaasRegistry } from "./registry.ts";
import { runTenantResolver } from "./resolver.ts";

interface EmailCommand {
  action: "account-create" | "account-resend" | "account-enable";
  email: string;
}

interface DisableCommand {
  action: "account-disable";
  email: string;
}

interface SimpleCommand {
  action: "reconcile" | "accounts" | "resolver" | "signup-init";
}

interface BackupCommand {
  action: "backup";
  email?: string;
}

interface RolloutCommand {
  action: "rollout";
  imageDigest: string;
}


type SaasCommand = EmailCommand | DisableCommand | SimpleCommand | BackupCommand | RolloutCommand;

function usageError(): never {
  throw new Error(
    "usage: roost saas account-create --email <address> | account-resend --email <address> | "
      + "account-disable --email <address> --yes | account-enable --email <address> | accounts | backup | "
      + "rollout --image <sha256:digest> | reconcile | resolver | signup-init",
  );
}

function parseEmailCommand(action: EmailCommand["action"], args: readonly string[]): EmailCommand {
  if (args.length !== 2 || args[0] !== "--email" || !args[1]) usageError();
  return { action, email: args[1] };
}

export function parseSaasCommand(args: readonly string[]): SaasCommand {
  const [action, ...rest] = args;
  if (action === "account-create" || action === "account-resend" || action === "account-enable") {
    return parseEmailCommand(action, rest);
  }
  if (action === "account-disable") {
    if (rest.length !== 3 || rest[0] !== "--email" || !rest[1] || rest[2] !== "--yes") usageError();
    return { action, email: rest[1] };
  }
  if (
    (action === "reconcile" || action === "accounts" || action === "resolver" || action === "signup-init")
    && rest.length === 0
  ) {
    return { action };
  }
  if (action === "backup") {
    if (rest.length === 0) return { action };
    if (rest.length === 2 && rest[0] === "--email" && rest[1]) return { action, email: rest[1] };
  }
  if (action === "rollout") {
    if (rest.length === 2 && rest[0] === "--image" && rest[1]) {
      return { action, imageDigest: rest[1] };
    }
  }
  return usageError();
}

function databaseBytes(dataDir: string): number {
  let total = 0;
  for (const name of ["coordinator_v2.db", "coordinator_v2.db-wal", "coordinator_v2.db-shm"]) {
    const path = join(dataDir, name);
    if (existsSync(path)) total += statSync(path).size;
  }
  return total;
}

function lastBackupAgeMs(rootDir: string, coordinatorId: string): number | null {
  const dir = join(rootDir, "backups", coordinatorId);
  if (!existsSync(dir)) return null;
  const latest = readdirSync(dir)
    .filter((name) => name.endsWith(".tar.age"))
    .map((name) => statSync(join(dir, name)).mtimeMs)
    .sort((a, b) => b - a)[0];
  return latest === undefined ? null : Math.max(0, Date.now() - latest);
}

export async function saas(args: string[]): Promise<void> {
  const command = parseSaasCommand(args);
  if (command.action === "resolver") {
    await runTenantResolver();
    return;
  }
  if (command.action === "signup-init") {
    const { runSignupInit } = await import("../saas-auth/signup-init.ts");
    const result = runSignupInit();
    console.log(JSON.stringify({
      event: "saas.signup_initialized",
      credential_directory: result.credentialDirectory,
      assertion_verify_key_path: result.assertionVerifyKeyPath,
    }));
    return;
  }
  const config = loadSaasHostConfig(process.env as Record<string, string | undefined>);
  if (
    command.action === "account-create"
    || command.action === "account-resend"
    || command.action === "account-enable"
    || command.action === "reconcile"
    || command.action === "rollout"
  ) {
    await assertSaasHostPrerequisites(
      command.action === "rollout" ? { ...config, imageDigest: command.imageDigest } : config,
    );
  }

  const registry = new SaasRegistry({ rootDir: config.rootDir, path: config.registryPath });
  try {
    const runtime = new ManagedInstanceRuntime({ network: config.network });
    const caddy = new CaddyTenantRouter({ confDir: config.caddyConfDir });
    const probe = new ManagedRouteProbe();
    const routes: TenantRouteManager = {
      reconcile: (coordinators) => caddy.reconcile(coordinators),
      verify: (coordinator) => probe.verify(coordinator),
      verifyResolver: (account) => probe.verifyResolver(account),
    };
    const admission = new HostAdmission({
      registry,
      config,
      onAlert: (message) => console.error(JSON.stringify({ event: "saas.admission_warning", message })),
    });
    const lifecycle = new SaasLifecycle({
      registry,
      runtime,
      routes,
      admission,
      email: {
        resendEndpoint: config.resendEndpoint,
        emailFrom: config.emailFrom,
        sharedResendApiKeyPath: config.sharedResendApiKeyPath,
      },
      authVerifyKeyFile: config.authVerifyKeyFile,
    });

    if (command.action === "account-create") {
      const result = await lifecycle.accountCreate(command.email, config.imageDigest);
      console.log(JSON.stringify({
        event: "saas.account_create",
        resumed: result.resumed,
        account_id: result.account.id,
        coordinator_id: result.coordinator.id,
        dashboard_url: MANAGED_WEB_PUBLIC_ORIGIN,
        state: result.coordinator.state,
        image_digest: result.coordinator.imageDigest,
      }));
      return;
    }
    if (command.action === "account-resend") {
      const result = await lifecycle.accountResend(command.email);
      console.log(JSON.stringify({
        event: "saas.account_resend",
        account_id: result.account.id,
        coordinator_id: result.coordinator.id,
        dashboard_url: MANAGED_WEB_PUBLIC_ORIGIN,
        state: result.coordinator.state,
      }));
      return;
    }
    if (command.action === "account-disable") {
      const result = await lifecycle.accountDisable(command.email);
      console.log(JSON.stringify({
        event: "saas.account_disable",
        account_id: result.account.id,
        coordinator_id: result.coordinator.id,
        dashboard_url: MANAGED_WEB_PUBLIC_ORIGIN,
        state: result.coordinator.state,
      }));
      return;
    }
    if (command.action === "account-enable") {
      const result = await lifecycle.accountEnable(command.email);
      console.log(JSON.stringify({
        event: "saas.account_enable",
        account_id: result.account.id,
        coordinator_id: result.coordinator.id,
        dashboard_url: MANAGED_WEB_PUBLIC_ORIGIN,
        state: result.coordinator.state,
      }));
      return;
    }
    if (command.action === "accounts") {
      const accounts = [];
      for (const account of registry.listAccounts()) {
        for (const coordinator of registry.listCoordinators().filter((row) => row.accountId === account.id)) {
          accounts.push({
            account_id: account.id,
            coordinator_id: coordinator.id,
            email_normalized: account.emailNormalized,
            dashboard_url: MANAGED_WEB_PUBLIC_ORIGIN,
            account_state: account.state,
            coordinator_state: coordinator.state,
            image_digest: coordinator.imageDigest,
            health: await runtime.containerHealth(coordinator),
            activation_age_ms: Math.max(0, Date.now() - (coordinator.invitedAtMs ?? coordinator.createdAtMs)),
            db_usage_bytes: databaseBytes(coordinator.dataDir),
            last_backup_age_ms: lastBackupAgeMs(config.rootDir, coordinator.id),
          });
        }
      }
      console.log(JSON.stringify({ event: "saas.accounts", accounts }));
      return;
    }
    if (command.action === "backup") {
      const selectedAccount = command.email ? registry.getAccountByEmail(command.email) : null;
      if (command.email && !selectedAccount) throw new Error("account not found");
      const artifacts = [];
      const failures = [];
      for (const coordinator of registry.listCoordinators().filter(
        (row) => selectedAccount === null || row.accountId === selectedAccount.id,
      )) {
        if (coordinator.state === "reserved") continue;
        const owner = `backup-${process.pid}-${coordinator.id}`;
        let leased = false;
        try {
          registry.acquireLease(coordinator.id, "backup", owner, 60 * 60_000);
          leased = true;
          artifacts.push({
            coordinator_id: coordinator.id,
            path: await createEncryptedBackup(coordinator, {
              rootDir: config.rootDir,
              ageRecipient: config.ageRecipient,
              ageIdentityFile: config.ageIdentityFile,
            }),
          });
        } catch (error) {
          failures.push({
            coordinator_id: coordinator.id,
            error: error instanceof Error ? error.message : "backup failed",
          });
        } finally {
          if (leased) {
            try { registry.releaseLease(coordinator.id, "backup", owner); }
            catch { /* the failure report remains authoritative */ }
          }
        }
      }
      console.log(JSON.stringify({ event: "saas.backup", artifacts, failures }));
      if (failures.length > 0) throw new Error(`${failures.length} coordinator backup(s) failed`);
      return;
    }
    if (command.action === "rollout") {
      const rollout = new SaasRollout({
        registry,
        runtime,
        routes,
        admission,
        email: {
          resendEndpoint: config.resendEndpoint,
          emailFrom: config.emailFrom,
          sharedResendApiKeyPath: config.sharedResendApiKeyPath,
        },
        authVerifyKeyFile: config.authVerifyKeyFile,
        backup: (coordinator) => createEncryptedBackup(coordinator, {
          rootDir: config.rootDir,
          ageRecipient: config.ageRecipient,
          ageIdentityFile: config.ageIdentityFile,
        }),
      });
      console.log(JSON.stringify({
        event: "saas.rollout",
        coordinators: await rollout.rollout(command.imageDigest),
      }));
      return;
    }
    const results = await lifecycle.reconcile();
    console.log(JSON.stringify({ event: "saas.reconcile", coordinators: results }));
  } finally {
    registry.close();
  }
}
