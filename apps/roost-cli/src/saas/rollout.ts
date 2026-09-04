// Replaces managed coordinator images with rollback-safe SQLite snapshots.
// Operator rollout commands use this module under coordinator and route leases.
// Container renames and registry commits preserve the prior image on failed health checks.
import { Database } from "bun:sqlite";
import { chmodSync, existsSync, renameSync, rmSync } from "node:fs";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import { createSqliteSnapshot } from "../../../coord/src/db/snapshot.ts";
import { assertImmutableImageDigest, SaasRegistry, type RegistryCoordinator } from "./registry.ts";
import type { ManagedEmailRuntimeConfig, ManagedInstanceSpec } from "./docker.ts";
import type { ManagedRuntimePort, ProvisioningAdmission, TenantRouteManager } from "./lifecycle.ts";

const ROLLOUT_LEASE_MS = 60 * 60_000;
const ROUTE_LEASE_MS = 5 * 60_000;
const ROLLBACK_DATABASE = ".roost-rollout-rollback.db";

export interface RolloutRuntimePort extends ManagedRuntimePort {
  renameContainer(from: string, to: string): Promise<void>;
  removeContainer(name: string): Promise<void>;
}

export interface SaasRolloutOptions {
  registry: SaasRegistry;
  runtime: RolloutRuntimePort;
  routes: TenantRouteManager;
  admission: ProvisioningAdmission;
  email: ManagedEmailRuntimeConfig;
  authVerifyKeyFile: string;
  backup: (coordinator: RegistryCoordinator) => Promise<string>;
  leaseOwner?: () => string;
}

export interface RolloutResult {
  coordinatorId: string;
  priorDigest: string;
  imageDigest: string;
  rolledBack: boolean;
  error?: string;
}

function safeError(error: unknown): string {
  return (error instanceof Error ? error.message : String(error))
    .replace(/(?:Bearer\s+|token=)[^\s&#]+/gi, "[credential]")
    .replace(/[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}/gi, "[email]")
    .replace(/[A-Za-z0-9_-]{40,}/g, "[secret]")
    .replace(/[\u0000-\u001f\u007f-\u009f]/gu, " ")
    .slice(0, 1_024);
}

function createRollbackDatabaseSnapshot(coordinator: RegistryCoordinator): string {
  const databasePath = join(coordinator.dataDir, "coordinator_v2.db");
  const snapshotPath = join(coordinator.dataDir, ROLLBACK_DATABASE);
  rmSync(snapshotPath, { force: true });
  const sqlite = new Database(databasePath, { readonly: true });
  try { createSqliteSnapshot(sqlite, snapshotPath); }
  finally { sqlite.close(true); }
  chmodSync(snapshotPath, 0o600);
  return snapshotPath;
}

function restoreRollbackDatabase(coordinator: RegistryCoordinator, snapshotPath: string): void {
  if (!existsSync(snapshotPath)) throw new Error("rollout rollback database snapshot is missing");
  const databasePath = join(coordinator.dataDir, "coordinator_v2.db");
  rmSync(`${databasePath}-wal`, { force: true });
  rmSync(`${databasePath}-shm`, { force: true });
  rmSync(databasePath, { force: true });
  renameSync(snapshotPath, databasePath);
  chmodSync(databasePath, 0o600);
}

export class SaasRollout {
  private readonly registry: SaasRegistry;
  private readonly runtime: RolloutRuntimePort;
  private readonly routes: TenantRouteManager;
  private readonly admission: ProvisioningAdmission;
  private readonly email: ManagedEmailRuntimeConfig;
  private readonly authVerifyKeyFile: string;
  private readonly backup: (coordinator: RegistryCoordinator) => Promise<string>;
  private readonly leaseOwner: () => string;

  constructor(options: SaasRolloutOptions) {
    this.registry = options.registry;
    this.runtime = options.runtime;
    this.routes = options.routes;
    this.admission = options.admission;
    this.email = options.email;
    this.authVerifyKeyFile = options.authVerifyKeyFile;
    this.backup = options.backup;
    this.leaseOwner = options.leaseOwner ?? randomUUID;
  }

  private routedRows(replacement?: RegistryCoordinator, excludedId?: string): RegistryCoordinator[] {
    const states = ["routed", "invited", "active"] as const;
    const rows = this.registry.listCoordinators(states)
      .filter((row) => row.id !== excludedId && row.id !== replacement?.id);
    if (replacement) rows.push(replacement);
    return rows.sort((a, b) => a.hostname.localeCompare(b.hostname));
  }

  private spec(coordinator: RegistryCoordinator): ManagedInstanceSpec {
    return {
      account: this.registry.getAccount(coordinator.accountId),
      coordinator,
      authVerifyKeyFile: this.authVerifyKeyFile,
      email: this.email,
    };
  }

  async rollout(imageDigestRaw: string, accountEmail?: string): Promise<RolloutResult[]> {
    const imageDigest = assertImmutableImageDigest(imageDigestRaw);
    await this.admission.assertPendingWorkAllowed();
    const account = accountEmail ? this.registry.getAccountByEmail(accountEmail) : null;
    if (accountEmail && !account) throw new Error("account not found");
    const candidates = this.registry.listCoordinators(["active", "invited", "routed"])
      .filter((coordinator) => account === null || coordinator.accountId === account.id);
    const results: RolloutResult[] = [];

    for (const candidate of candidates) {
      if (candidate.imageDigest === imageDigest) {
        results.push({
          coordinatorId: candidate.id,
          priorDigest: candidate.imageDigest,
          imageDigest,
          rolledBack: false,
        });
        continue;
      }

      const owner = this.leaseOwner();
      this.registry.acquireLease(candidate.id, "rollout", owner, ROLLOUT_LEASE_MS);
      try {
        await this.runtime.recoverInterruptedReplacement?.(this.spec(candidate));
        await this.backup(candidate);
        const routeOwner = `${owner}-routes`;
        this.registry.acquireGlobalLease("tenant-routes", "rollout-route", routeOwner, ROUTE_LEASE_MS);
        let routeLeaseFailure: unknown = null;
        const routeHeartbeat = setInterval(() => {
          try {
            this.registry.renewGlobalLease("tenant-routes", "rollout-route", routeOwner, ROUTE_LEASE_MS);
          } catch (error) {
            routeLeaseFailure = error;
          }
        }, 60_000);
        routeHeartbeat.unref();
        try {
          const prior = this.registry.getCoordinator(candidate.id);
          const replacement: RegistryCoordinator = { ...prior, imageDigest };
          const rollbackName = `${prior.containerName}-rollback`;
          let renamed = false;
          let replacementStarted = false;
          let committed = false;
          let rollbackDatabase: string | null = null;
          try {
            await this.routes.reconcile(this.routedRows(undefined, prior.id));
            await this.runtime.stop(prior);
            rollbackDatabase = createRollbackDatabaseSnapshot(prior);
            await this.runtime.renameContainer(prior.containerName, rollbackName);
            renamed = true;
            await this.runtime.ensureContainer(this.spec(replacement));
            await this.runtime.startAndVerify(this.spec(replacement));
            replacementStarted = true;
            await this.routes.reconcile(this.routedRows(replacement));
            await this.routes.verify(replacement);
            this.registry.updateCoordinatorImageDigest(prior.id, prior.imageDigest, imageDigest);
            committed = true;
          } catch (error) {
            if (committed) throw error;
            const rollbackErrors: string[] = [];
            if (replacementStarted || renamed) {
              try { await this.runtime.removeContainer(prior.containerName); }
              catch (cleanupError) { rollbackErrors.push(safeError(cleanupError)); }
            }
            if (rollbackDatabase !== null) {
              try { restoreRollbackDatabase(prior, rollbackDatabase); }
              catch (databaseError) { rollbackErrors.push(safeError(databaseError)); }
            }
            if (renamed) {
              try { await this.runtime.renameContainer(rollbackName, prior.containerName); }
              catch (renameError) { rollbackErrors.push(safeError(renameError)); }
            }
            try {
              await this.runtime.startAndVerify(this.spec(prior));
              await this.routes.reconcile(this.routedRows(prior));
              await this.routes.verify(prior);
            } catch (restoreError) {
              rollbackErrors.push(safeError(restoreError));
            }
            const failure = safeError(error);
            if (rollbackErrors.length > 0) {
              throw new Error(`${failure}; rollback failed: ${rollbackErrors.join("; ")}`);
            }
            results.push({
              coordinatorId: prior.id,
              priorDigest: prior.imageDigest,
              imageDigest: prior.imageDigest,
              rolledBack: true,
              error: failure,
            });
            continue;
          }

          try { await this.runtime.removeContainer(rollbackName); }
          catch (error) { this.registry.setCoordinatorError(prior.id, `stale rollback container: ${safeError(error)}`); }
          if (rollbackDatabase !== null) rmSync(rollbackDatabase, { force: true });
          results.push({
            coordinatorId: prior.id,
            priorDigest: prior.imageDigest,
            imageDigest,
            rolledBack: false,
          });
        } finally {
          clearInterval(routeHeartbeat);
          if (!routeLeaseFailure) {
            try {
              this.registry.renewGlobalLease(
                "tenant-routes",
                "rollout-route",
                routeOwner,
                ROUTE_LEASE_MS,
              );
            } catch (error) {
              routeLeaseFailure = error;
            }
          }
          try { this.registry.releaseGlobalLease("tenant-routes", "rollout-route", routeOwner); }
          catch { /* a lost route lease makes the rollout result unsafe */ }
          if (routeLeaseFailure) throw routeLeaseFailure;
        }
      } finally {
        try { this.registry.releaseLease(candidate.id, "rollout", owner); }
        catch { /* stale lease cleanup is reconciled later */ }
      }
    }
    return results;
  }
}
