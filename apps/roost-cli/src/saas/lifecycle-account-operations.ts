// Owns operator and provisioning commands for individual SaaS accounts.
// CLI commands and the provisioning worker call these inherited lifecycle operations.
// Each command re-reads registry state before crossing a managed side-effect boundary.
import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import type { ActivationStatus } from "./docker.ts";
import type { AccountReservation, RegistryAccount, RegistryCoordinator } from "./registry.ts";
import {
  ROUTED_STATES,
  redactLifecycleError,
  specFor,
  type ProvisioningBootstrap,
  type ProvisioningResult,
} from "./lifecycle-contract.ts";
import { LifecycleCore } from "./lifecycle-core.ts";

export class LifecycleAccountOperations extends LifecycleCore {
  async proveActive(
    accountId: string,
    coordinatorId: string,
    expectedTopologies: readonly ActivationStatus["topology"][] = [
      "active-native-password",
      "active-passwordless-google",
      "active-linked",
    ],
  ): Promise<ProvisioningResult> {
    const initial = this.registry.getCoordinator(coordinatorId);
    const account = this.registry.getAccount(accountId);
    if (initial.accountId !== account.id
      || initial.routeKey !== account.routeKey
      || initial.state !== "active"
      || account.state !== "active") {
      throw new Error("active account proof binding mismatch");
    }
    const coordinator = await this.withLease(initial, "provision-proof", async () => {
      const current = this.registry.getCoordinator(initial.id);
      const spec = specFor(account, current, this.email, this.authVerifyKeyFile);
      await this.runtime.recoverInterruptedReplacement?.(spec);
      await this.runtime.ensureContainer(spec);
      await this.runtime.startAndVerify(spec);
      await this.repairAndVerifyRoute(account, current, "route-proof-active");
      const status = await this.runtime.activationStatus(current);
      if (!status.activated
        || status.accountId !== account.id
        || status.coordinatorId !== current.id
        || !expectedTopologies.includes(status.topology)) {
        throw new Error("active account topology proof mismatch");
      }
      await this.routes.verifyResolver(account);
      return current;
    });
    return { account, coordinator, resumed: true };
  }

  async provision(
    reservation: AccountReservation,
    bootstrap: ProvisioningBootstrap,
  ): Promise<ProvisioningResult> {
    const account = this.registry.getAccount(reservation.account.id);
    const initial = this.registry.getCoordinator(reservation.coordinator.id);
    if (initial.accountId !== account.id
      || account.routeKey !== initial.routeKey
      || reservation.account.id !== account.id
      || reservation.coordinator.id !== initial.id) {
      throw new Error("provisioning reservation binding mismatch");
    }
    try {
      const coordinator = await this.withLease(
        initial,
        `provision-${bootstrap.kind}`,
        () => this.advance(account, this.registry.getCoordinator(initial.id), bootstrap),
      );
      return {
        account: this.registry.getAccount(account.id),
        coordinator,
        resumed: reservation.resumed,
      };
    } catch (error) {
      try { this.registry.setCoordinatorError(initial.id, redactLifecycleError(error)); }
      catch { /* the original failure is authoritative */ }
      throw error;
    }
  }

  async accountCreate(emailRaw: string, imageDigest: string): Promise<ProvisioningResult> {
    const email = normalizeAccountEmail(emailRaw);
    if (!email) throw new Error("invalid account email");
    const reservation: AccountReservation = await this.admission.assertBeforeReservation(
      () => this.registry.reserveAccount(email, imageDigest),
    );
    try {
      const coordinator = await this.withLease(reservation.coordinator, "account-create", () =>
        this.advance(
          reservation.account,
          this.registry.getCoordinator(reservation.coordinator.id),
          { kind: "operator-email" },
        )
      );
      return {
        account: this.registry.getAccount(reservation.account.id),
        coordinator,
        resumed: reservation.resumed,
      };
    } catch (error) {
      try { this.registry.setCoordinatorError(reservation.coordinator.id, redactLifecycleError(error)); }
      catch { /* the original failure is authoritative */ }
      throw error;
    }
  }

  async accountResend(emailRaw: string): Promise<ProvisioningResult> {
    const email = normalizeAccountEmail(emailRaw);
    if (!email) throw new Error("invalid account email");
    const account = this.registry.getAccountByEmail(email);
    if (!account) throw new Error("pending account not found");
    if (account.state !== "pending") throw new Error(`account ${email} is not pending`);
    const coordinator = this.registry.listCoordinators().find(
      (candidate) => candidate.accountId === account.id && candidate.ordinal === 1,
    );
    if (!coordinator) throw new Error("pending account has no coordinator");
    await this.admission.assertPendingWorkAllowed();
    try {
      const result = await this.withLease(coordinator, "account-resend", async () => {
        const currentAccount = this.registry.getAccount(account.id);
        if (currentAccount.state !== "pending") {
          throw new Error(`account ${email} is not pending`);
        }
        const current = this.registry.getCoordinator(coordinator.id);
        if (current.accountId !== currentAccount.id
          || current.state === "active"
          || current.state === "disabled") {
          throw new Error("pending account coordinator state is inconsistent");
        }
        if (current.state === "invited" || current.state === "routed") {
          await this.runtime.ensureContainer(specFor(currentAccount, current, this.email, this.authVerifyKeyFile));
          await this.runtime.startAndVerify(specFor(currentAccount, current, this.email, this.authVerifyKeyFile));
          const status = await this.runtime.activationStatus(current);
          if (status.activated) {
            if (status.accountId !== currentAccount.id) throw new Error("activated account identity mismatch");
            return this.registry.markActivationCommitted(currentAccount.id, current.id);
          }
        }
        const seeded = await this.withRouteLease("route-remove-resend", async () => {
          if (ROUTED_STATES.includes(current.state as typeof ROUTED_STATES[number])) {
            await this.routes.reconcile(this.routedRows(undefined, current.id));
          }
          await this.runtime.stop(current);
          const reserved = current.state === "reserved"
            ? current
            : this.registry.transitionCoordinator(current.id, current.state, "reserved");
          await this.runtime.seedOwnerActivation(specFor(currentAccount, reserved, this.email, this.authVerifyKeyFile));
          return this.registry.transitionCoordinator(reserved.id, "reserved", "seeded");
        });
        return this.advance(currentAccount, seeded, { kind: "operator-email" });
      });
      return { account: this.registry.getAccount(account.id), coordinator: result, resumed: true };
    } catch (error) {
      try { this.registry.setCoordinatorError(coordinator.id, redactLifecycleError(error)); }
      catch { /* preserve original */ }
      throw error;
    }
  }

  async accountDisable(emailRaw: string): Promise<ProvisioningResult> {
    const email = normalizeAccountEmail(emailRaw);
    if (!email) throw new Error("invalid account email");
    const account = this.registry.getAccountByEmail(email);
    if (!account) throw new Error("account not found");
    if (account.state === "disabled") throw new Error(`account ${email} is already disabled`);
    const coordinator = this.registry.listCoordinators().find(
      (candidate) => candidate.accountId === account.id && candidate.ordinal === 1,
    );
    if (!coordinator) throw new Error("account has no coordinator");
    const disabled = await this.withLease(coordinator, "account-disable", () =>
      this.withRouteLease("route-remove-disable", async () => {
        const current = this.registry.getCoordinator(coordinator.id);
        await this.routes.reconcile(this.routedRows(undefined, current.id));
        await this.runtime.stop(current);
        return this.registry.disableAccountAndCoordinator(account.id, current.id);
      })
    );
    return { account: this.registry.getAccount(account.id), coordinator: disabled, resumed: false };
  }

  async accountEnable(emailRaw: string): Promise<ProvisioningResult> {
    const email = normalizeAccountEmail(emailRaw);
    if (!email) throw new Error("invalid account email");
    const account = this.registry.getAccountByEmail(email);
    if (!account) throw new Error("account not found");
    if (account.state !== "disabled") throw new Error(`account ${email} is not disabled`);
    const coordinator = this.registry.listCoordinators().find(
      (candidate) => candidate.accountId === account.id && candidate.ordinal === 1,
    );
    if (!coordinator || coordinator.state !== "disabled") {
      throw new Error("disabled account coordinator state is inconsistent");
    }
    const enabled = await this.withLease(coordinator, "account-enable", async () => {
      const current = this.registry.getCoordinator(coordinator.id);
      const runtimeAccount: RegistryAccount = { ...account, state: "pending" };
      let recoverable = false;
      try {
        await this.runtime.stop(current);
        if (account.activatedAtMs === null) {
          await this.runtime.seedOwnerActivation(specFor(runtimeAccount, current, this.email, this.authVerifyKeyFile));
        }
        await this.runtime.ensureContainer(specFor(runtimeAccount, current, this.email, this.authVerifyKeyFile));
        await this.runtime.startAndVerify(specFor(runtimeAccount, current, this.email, this.authVerifyKeyFile));
        const status = await this.runtime.activationStatus(current);
        if (status.accountId !== account.id) throw new Error("activated account identity mismatch");
        const targetState = status.activated ? "active" : "routed";
        return await this.withRouteLease("route-enable", async () => {
          const desired: RegistryCoordinator = { ...current, state: targetState };
          await this.routes.reconcile(this.routedRows(desired));
          let restored: RegistryCoordinator;
          try {
            await this.routes.verify(desired);
            restored = this.registry.restoreAccountAndCoordinator(
              account.id,
              current.id,
              status.activated,
            );
          } catch (error) {
            await this.routes.reconcile(this.routedRows(undefined, current.id));
            throw error;
          }
          recoverable = true;
          if (status.activated) return restored;
          await this.routes.verifyResolver(this.registry.getAccount(account.id));
          const invited = this.registry.transitionCoordinator(restored.id, "routed", "invited");
          await this.runtime.releaseOwnerActivationEmail(invited);
          return invited;
        });
      } catch (error) {
        if (recoverable) throw error;
        const rollbackErrors: unknown[] = [];
        try {
          await this.withRouteLease("route-rollback-enable", () =>
            this.routes.reconcile(this.routedRows(undefined, current.id))
          );
        } catch (rollbackError) {
          rollbackErrors.push(rollbackError);
        }
        try {
          await this.runtime.stop(current);
        } catch (rollbackError) {
          rollbackErrors.push(rollbackError);
        }
        if (rollbackErrors.length > 0) {
          throw new AggregateError(
            [error, ...rollbackErrors],
            "account enable failed and rollback did not complete",
          );
        }
        throw error;
      }
    });
    return { account: this.registry.getAccount(account.id), coordinator: enabled, resumed: true };
  }

}
