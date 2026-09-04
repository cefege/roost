// Owns shared lifecycle state, leases, routing, and forward stage advancement.
// Account operations and reconciliation inherit these ordered side-effect boundaries.
// Lease heartbeats and rollback ordering keep retries safe across interrupted commands.
import { randomUUID } from "node:crypto";
import {
  GOOGLE_IDENTITY_ISSUER,
  SaasRegistryError,
  type RegistryAccount,
  type RegistryCoordinator,
  type SaasRegistry,
} from "./registry.ts";
import type { ActivationStatus, ManagedEmailRuntimeConfig } from "./docker.ts";
import {
  OPERATION_LEASE_MS,
  ROUTED_STATES,
  ROUTE_LEASE_MS,
  specFor,
  type LifecycleBootstrap,
  type ManagedRuntimePort,
  type ProvisioningAdmission,
  type SaasLifecycleOptions,
  type TenantRouteManager,
} from "./lifecycle-contract.ts";

export class LifecycleCore {
  protected readonly registry: SaasRegistry;
  protected readonly runtime: ManagedRuntimePort;
  protected readonly routes: TenantRouteManager;
  protected readonly email: ManagedEmailRuntimeConfig;
  protected readonly authVerifyKeyFile: string;
  protected readonly admission: ProvisioningAdmission;
  protected readonly now: () => number;
  protected readonly leaseOwner: () => string;

  constructor(options: SaasLifecycleOptions) {
    this.registry = options.registry;
    this.runtime = options.runtime;
    this.routes = options.routes;
    this.email = options.email;
    this.authVerifyKeyFile = options.authVerifyKeyFile;
    this.admission = options.admission;
    this.now = options.now ?? Date.now;
    this.leaseOwner = options.leaseOwner ?? randomUUID;
  }

  protected routedRows(replacement?: RegistryCoordinator, excludedId?: string): RegistryCoordinator[] {
    const rows = this.registry.listCoordinators(ROUTED_STATES);
    const filtered = rows.filter((row) => row.id !== excludedId && row.id !== replacement?.id);
    if (replacement) filtered.push(replacement);
    return filtered.sort((a, b) => a.routeKey.localeCompare(b.routeKey));
  }

  protected async pendingEffect<T>(
    account: RegistryAccount,
    effect: () => Promise<T>,
  ): Promise<T> {
    if (account.state === "pending") await this.admission.assertPendingWorkAllowed();
    return effect();
  }

  private assertActivationStatus(
    account: RegistryAccount,
    coordinator: RegistryCoordinator,
    status: ActivationStatus,
    bootstrap: LifecycleBootstrap,
  ): void {
    if (status.accountId !== account.id || status.coordinatorId !== coordinator.id) {
      throw new Error("activated account identity mismatch");
    }
    const expectedTopology = bootstrap.kind === "google"
      ? "active-passwordless-google"
      : "active-native-password";
    if (!status.activated || status.topology !== expectedTopology) {
      throw new Error("managed credential topology proof mismatch");
    }
  }

  protected async installAndVerifyRoute(
    account: RegistryAccount,
    coordinator: RegistryCoordinator,
  ): Promise<RegistryCoordinator> {
    return this.withRouteLease("route-install", async () => {
      const desired: RegistryCoordinator = { ...coordinator, state: "routed" };
      const prior = this.routedRows(undefined, coordinator.id);
      await this.pendingEffect(account, () => this.routes.reconcile([...prior, desired]));
      try {
        await this.routes.verify(desired);
      } catch (error) {
        await this.routes.reconcile(prior);
        throw error;
      }
      return this.registry.transitionCoordinator(coordinator.id, "running", "routed");
    });
  }

  protected async repairAndVerifyRoute(
    account: RegistryAccount,
    coordinator: RegistryCoordinator,
    operation: string,
  ): Promise<void> {
    await this.withRouteLease(operation, async () => {
      await this.pendingEffect(
        account,
        () => this.routes.reconcile(this.routedRows(coordinator)),
      );
      await this.routes.verify(coordinator);
    });
  }

  protected async commitActivation(
    account: RegistryAccount,
    coordinator: RegistryCoordinator,
  ): Promise<RegistryCoordinator> {
    return this.withRouteLease("route-activate", async () => {
      const desired: RegistryCoordinator = { ...coordinator, state: "active" };
      await this.pendingEffect(
        account,
        () => this.routes.reconcile(this.routedRows(desired)),
      );
      await this.routes.verify(desired);
      await this.routes.verifyResolver(account);
      return this.registry.markActivationCommitted(account.id, coordinator.id);
    });
  }

  protected async advance(
    account: RegistryAccount,
    initial: RegistryCoordinator,
    bootstrap: LifecycleBootstrap,
  ): Promise<RegistryCoordinator> {
    let coordinator = initial;
    for (;;) {
      const spec = specFor(account, coordinator, this.email, this.authVerifyKeyFile);
      if (this.runtime.recoverInterruptedReplacement) {
        await this.pendingEffect(
          account,
          () => this.runtime.recoverInterruptedReplacement!(spec),
        );
      }
      switch (coordinator.state) {
        case "reserved":
          if (bootstrap.kind === "verified-email") {
            await this.pendingEffect(
              account,
              () => this.runtime.seedSignupGatewayOwnerActivation(
                spec,
                bootstrap.activationTokenHash,
              ),
            );
          } else if (bootstrap.kind === "google") {
            await this.pendingEffect(
              account,
              () => this.runtime.seedGoogleOwner(spec, bootstrap.subject),
            );
          } else {
            await this.pendingEffect(account, () => this.runtime.seedOwnerActivation(spec));
          }
          coordinator = this.registry.transitionCoordinator(coordinator.id, "reserved", "seeded");
          continue;
        case "seeded":
          await this.pendingEffect(account, () => this.runtime.ensureContainer(spec));
          await this.pendingEffect(account, () => this.runtime.startAndVerify(spec));
          coordinator = this.registry.transitionCoordinator(coordinator.id, "seeded", "running");
          continue;
        case "running":
          await this.pendingEffect(account, () => this.runtime.ensureContainer(spec));
          await this.pendingEffect(account, () => this.runtime.startAndVerify(spec));
          coordinator = await this.installAndVerifyRoute(account, coordinator);
          continue;
        case "routed": {
          await this.pendingEffect(account, () => this.runtime.ensureContainer(spec));
          await this.pendingEffect(account, () => this.runtime.startAndVerify(spec));
          await this.repairAndVerifyRoute(account, coordinator, "route-repair-routed");
          const status = await this.runtime.activationStatus(coordinator);
          if (bootstrap.kind === "google") {
            this.assertActivationStatus(account, coordinator, status, bootstrap);
            coordinator = await this.commitActivation(account, coordinator);
            this.registry.activateFederatedIdentity(
              GOOGLE_IDENTITY_ISSUER,
              bootstrap.subject,
              account.id,
            );
            return coordinator;
          }
          if (status.activated) {
            this.assertActivationStatus(account, coordinator, status, bootstrap);
            return this.commitActivation(account, coordinator);
          }
          await this.routes.verifyResolver(account);
          coordinator = this.registry.transitionCoordinator(coordinator.id, "routed", "invited");
          if (bootstrap.kind === "operator-email") {
            await this.pendingEffect(
              account,
              () => this.runtime.releaseOwnerActivationEmail(coordinator),
            );
          }
          return coordinator;
        }
        case "invited": {
          if (bootstrap.kind === "google") {
            throw new Error("Google bootstrap cannot enter invited state");
          }
          await this.pendingEffect(account, () => this.runtime.ensureContainer(spec));
          await this.pendingEffect(account, () => this.runtime.startAndVerify(spec));
          await this.repairAndVerifyRoute(account, coordinator, "route-repair-invited");
          const status = await this.runtime.activationStatus(coordinator);
          if (!status.activated) {
            await this.routes.verifyResolver(account);
            if (bootstrap.kind === "operator-email") {
              await this.pendingEffect(
                account,
                () => this.runtime.releaseOwnerActivationEmail(coordinator),
              );
            }
            return coordinator;
          }
          this.assertActivationStatus(account, coordinator, status, bootstrap);
          return this.commitActivation(account, coordinator);
        }
        case "active": {
          await this.runtime.ensureContainer(spec);
          await this.runtime.startAndVerify(spec);
          await this.repairAndVerifyRoute(account, coordinator, "route-repair-active");
          const status = await this.runtime.activationStatus(coordinator);
          this.assertActivationStatus(account, coordinator, status, bootstrap);
          await this.routes.verifyResolver(account);
          if (bootstrap.kind === "google") {
            this.registry.activateFederatedIdentity(
              GOOGLE_IDENTITY_ISSUER,
              bootstrap.subject,
              account.id,
            );
          }
          return coordinator;
        }
        case "disabled":
          throw new Error("coordinator is disabled");
        case "failed":
          throw new Error("coordinator requires operator recovery");
      }
    }
  }

  protected async withLease<T>(
    coordinator: RegistryCoordinator,
    operation: string,
    action: () => Promise<T>,
  ): Promise<T> {
    const owner = this.leaseOwner();
    this.registry.acquireLease(coordinator.id, operation, owner, OPERATION_LEASE_MS);
    let leaseFailure: unknown = null;
    const heartbeat = setInterval(() => {
      try { this.registry.renewLease(coordinator.id, operation, owner, OPERATION_LEASE_MS); }
      catch (error) { leaseFailure = error; }
    }, 60_000);
    heartbeat.unref();
    try {
      const result = await action();
      if (leaseFailure) throw leaseFailure;
      this.registry.renewLease(coordinator.id, operation, owner, OPERATION_LEASE_MS);
      return result;
    } finally {
      clearInterval(heartbeat);
      try { this.registry.releaseLease(coordinator.id, operation, owner); }
      catch { /* a lost lease prevents successful completion above */ }
    }
  }

  protected async withRouteLease<T>(operation: string, action: () => Promise<T>): Promise<T> {
    const owner = `${this.leaseOwner()}-${randomUUID()}`;
    const waitDeadline = Date.now() + ROUTE_LEASE_MS;
    for (;;) {
      try {
        this.registry.acquireGlobalLease("tenant-routes", operation, owner, ROUTE_LEASE_MS);
        break;
      } catch (error) {
        if (!(error instanceof SaasRegistryError)
          || error.code !== "lease-held"
          || Date.now() >= waitDeadline) throw error;
        await Bun.sleep(50);
      }
    }
    let leaseFailure: unknown = null;
    const heartbeat = setInterval(() => {
      try { this.registry.renewGlobalLease("tenant-routes", operation, owner, ROUTE_LEASE_MS); }
      catch (error) { leaseFailure = error; }
    }, 60_000);
    heartbeat.unref();
    try {
      const result = await action();
      if (leaseFailure) throw leaseFailure;
      this.registry.renewGlobalLease("tenant-routes", operation, owner, ROUTE_LEASE_MS);
      return result;
    } finally {
      clearInterval(heartbeat);
      try { this.registry.releaseGlobalLease("tenant-routes", operation, owner); }
      catch { /* a lost lease prevents successful completion above */ }
    }
  }
}
