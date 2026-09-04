// Reconciles durable coordinator state with containers, routes, and activation proof.
// Startup and operator repair paths sweep every registry coordinator through this class.
// Failures remain isolated per tenant and are redacted before durable storage.
import { SaasRegistryError } from "./registry.ts";
import {
  redactLifecycleError,
  specFor,
  type ReconcileResult,
} from "./lifecycle-contract.ts";
import { LifecycleAccountOperations } from "./lifecycle-account-operations.ts";

export class LifecycleReconciliation extends LifecycleAccountOperations {
  async reconcile(): Promise<ReconcileResult[]> {
    const results: ReconcileResult[] = [];
    await this.withRouteLease("route-sweep", () => this.routes.reconcile(this.routedRows()));
    for (const initial of this.registry.listCoordinators()) {
      if (initial.state === "disabled") {
        try {
          await this.withLease(initial, "reconcile-disabled", () =>
            this.runtime.stop(this.registry.getCoordinator(initial.id))
          );
          results.push({ coordinatorId: initial.id, state: initial.state, repaired: false });
        } catch (error) {
          const message = redactLifecycleError(error);
          try { this.registry.setCoordinatorError(initial.id, message); } catch { /* preserve report */ }
          results.push({ coordinatorId: initial.id, state: initial.state, repaired: false, error: message });
        }
        continue;
      }
      if (initial.state === "failed") {
        results.push({ coordinatorId: initial.id, state: initial.state, repaired: false });
        continue;
      }
      try {
        const result = await this.withLease(initial, "reconcile", async () => {
          let coordinator = this.registry.getCoordinator(initial.id);
          const account = this.registry.getAccount(coordinator.accountId);
          await this.runtime.recoverInterruptedReplacement?.(
            specFor(account, coordinator, this.email, this.authVerifyKeyFile),
          );
          if (coordinator.state === "seeded") {
            const spec = specFor(account, coordinator, this.email, this.authVerifyKeyFile);
            await this.pendingEffect(account, () => this.runtime.ensureContainer(spec));
            await this.pendingEffect(account, () => this.runtime.startAndVerify(spec));
            coordinator = this.registry.transitionCoordinator(coordinator.id, "seeded", "running");
          }
          if (coordinator.state === "running") {
            const spec = specFor(account, coordinator, this.email, this.authVerifyKeyFile);
            await this.pendingEffect(account, () => this.runtime.ensureContainer(spec));
            await this.pendingEffect(account, () => this.runtime.startAndVerify(spec));
            coordinator = await this.installAndVerifyRoute(account, coordinator);
          }
          if (coordinator.state === "active") {
            await this.runtime.ensureContainer(specFor(account, coordinator, this.email, this.authVerifyKeyFile));
            await this.runtime.startAndVerify(specFor(account, coordinator, this.email, this.authVerifyKeyFile));
            await this.withRouteLease("route-repair", async () => {
              await this.routes.reconcile(this.routedRows(coordinator));
              await this.routes.verify(coordinator);
              this.registry.setCoordinatorError(coordinator.id, null);
            });
            return coordinator;
          }
          if (coordinator.state === "routed") {
            const spec = specFor(account, coordinator, this.email, this.authVerifyKeyFile);
            await this.pendingEffect(account, () => this.runtime.ensureContainer(spec));
            await this.pendingEffect(account, () => this.runtime.startAndVerify(spec));
            await this.repairAndVerifyRoute(account, coordinator, "route-repair-routed");
            const status = await this.runtime.activationStatus(coordinator);
            if (status.accountId !== account.id || status.coordinatorId !== coordinator.id) {
              throw new Error("activated account identity mismatch");
            }
            if (status.activated) {
              if (status.topology !== "active-native-password"
                && status.topology !== "active-passwordless-google") {
                throw new Error("managed credential topology proof mismatch");
              }
              return this.commitActivation(account, coordinator);
            }
            if (status.topology !== "pending-coordinator-email"
              && status.topology !== "pending-signup-gateway") {
              throw new Error("pending credential topology proof mismatch");
            }
            await this.routes.verifyResolver(account);
            const invited = this.registry.transitionCoordinator(coordinator.id, "routed", "invited");
            if (status.topology === "pending-coordinator-email") {
              await this.pendingEffect(
                account,
                () => this.runtime.releaseOwnerActivationEmail(invited),
              );
            }
            return invited;
          }
          if (coordinator.state === "invited") {
            const spec = specFor(account, coordinator, this.email, this.authVerifyKeyFile);
            await this.pendingEffect(account, () => this.runtime.ensureContainer(spec));
            await this.pendingEffect(account, () => this.runtime.startAndVerify(spec));
            const status = await this.runtime.activationStatus(coordinator);
            if (status.accountId !== account.id || status.coordinatorId !== coordinator.id) {
              throw new Error("activated account identity mismatch");
            }
            if (status.activated) {
              if (status.topology !== "active-native-password") {
                throw new Error("managed credential topology proof mismatch");
              }
              return this.commitActivation(account, coordinator);
            }
            if (status.topology !== "pending-coordinator-email"
              && status.topology !== "pending-signup-gateway") {
              throw new Error("pending credential topology proof mismatch");
            }
            if (status.expiresAtMs <= this.now()) {
              return this.withRouteLease("route-remove-expired", async () => {
                await this.pendingEffect(
                  account,
                  () => this.routes.reconcile(this.routedRows(undefined, coordinator.id)),
                );
                await this.pendingEffect(account, () => this.runtime.stop(coordinator));
                return this.registry.transitionCoordinator(
                  coordinator.id,
                  "invited",
                  "failed",
                  "owner activation expired",
                );
              });
            }
            await this.repairAndVerifyRoute(account, coordinator, "route-repair-invited");
            await this.routes.verifyResolver(account);
            if (status.topology === "pending-coordinator-email") {
              await this.pendingEffect(
                account,
                () => this.runtime.releaseOwnerActivationEmail(coordinator),
              );
            }
            return coordinator;
          }
          return coordinator;
        });
        results.push({ coordinatorId: result.id, state: result.state, repaired: result.state !== initial.state });
      } catch (error) {
        const current = this.registry.getCoordinator(initial.id);
        if (error instanceof SaasRegistryError && error.code === "lease-held") {
          results.push({
            coordinatorId: initial.id,
            state: current.state,
            repaired: current.state !== initial.state,
            error: "operation lease held",
          });
          continue;
        }
        const message = redactLifecycleError(error);
        try { this.registry.setCoordinatorError(initial.id, message); } catch { /* preserve report */ }
        results.push({
          coordinatorId: initial.id,
          state: current.state,
          repaired: current.state !== initial.state,
          error: message,
        });
      }
    }
    return results;
  }
}
