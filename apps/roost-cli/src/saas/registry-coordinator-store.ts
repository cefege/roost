// Owns registry account and coordinator queries plus coupled state mutations.
// Reservation, lifecycle, and lease stores inherit these canonical lookups.
// Coupled updates stay transactional so account and coordinator state cannot diverge.
import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import { RegistryStorage } from "./registry-storage.ts";
import { immediate } from "./registry-schema.ts";
import {
  type AccountState,
  type CoordinatorState,
  type RegistryAccount,
  type RegistryCoordinator,
} from "./registry-model.ts";
import type { RawAccount, RawCoordinator } from "./registry-row-types.ts";
import { mapAccount, mapCoordinator } from "./registry-row-mappers.ts";
import {
  MAX_ERROR_BYTES,
  ROUTE_KEY_RE,
  SaasRegistryError,
  assertCanonicalUuid,
  assertImmutableImageDigest,
  checkedNow,
  isCoordinatorState,
} from "./registry-validation.ts";

export class RegistryCoordinatorStore extends RegistryStorage {
  getAccount(id: string): RegistryAccount {
    assertCanonicalUuid(id, "account id");
    const raw = this.sqlite.query("SELECT * FROM accounts WHERE id = ?").get(id) as RawAccount | null;
    if (!raw) throw new SaasRegistryError("account not found", "not-found");
    return mapAccount(raw);
  }

  getAccountByEmail(emailRaw: string): RegistryAccount | null {
    const email = normalizeAccountEmail(emailRaw);
    if (!email) throw new SaasRegistryError("invalid account email", "invalid");
    const raw = this.sqlite.query("SELECT * FROM accounts WHERE email_normalized = ?").get(email) as RawAccount | null;
    return raw ? mapAccount(raw) : null;
  }

  getRouteKeyByEmail(emailRaw: string): string | null {
    const email = normalizeAccountEmail(emailRaw);
    if (!email) throw new SaasRegistryError("invalid account email", "invalid");
    const raw = this.sqlite.query(
      "SELECT route_key FROM accounts WHERE email_normalized = ?",
    ).get(email) as { route_key: unknown } | null;
    if (!raw) return null;
    if (typeof raw.route_key !== "string" || !ROUTE_KEY_RE.test(raw.route_key)) {
      throw new SaasRegistryError("registry row has invalid tenant route key", "corrupt");
    }
    return raw.route_key;
  }

  getCoordinator(id: string): RegistryCoordinator {
    assertCanonicalUuid(id, "coordinator id");
    const raw = this.sqlite.query(`
      SELECT c.*, a.route_key
      FROM coordinators c
      JOIN accounts a ON a.id = c.account_id
      WHERE c.id = ?
    `).get(id) as RawCoordinator | null;
    if (!raw) throw new SaasRegistryError("coordinator not found", "not-found");
    return mapCoordinator(raw, this.rootDir);
  }

  listAccounts(): RegistryAccount[] {
    return (this.sqlite.query("SELECT * FROM accounts ORDER BY created_at_ms, id").all() as RawAccount[])
      .map(mapAccount);
  }

  listCoordinators(states?: readonly CoordinatorState[]): RegistryCoordinator[] {
    const rows = this.sqlite.query(`
      SELECT c.*, a.route_key
      FROM coordinators c
      JOIN accounts a ON a.id = c.account_id
      ORDER BY c.created_at_ms, c.id
    `).all() as RawCoordinator[];
    const allowed = states ? new Set(states) : null;
    return rows.map((row) => mapCoordinator(row, this.rootDir))
      .filter((row) => allowed === null || allowed.has(row.state));
  }

  transitionCoordinator(
    id: string,
    expected: CoordinatorState | readonly CoordinatorState[],
    next: CoordinatorState,
    lastError: string | null = null,
  ): RegistryCoordinator {
    assertCanonicalUuid(id, "coordinator id");
    if (!isCoordinatorState(next)) throw new SaasRegistryError("invalid next coordinator state", "invalid");
    const expectedStates = Array.isArray(expected) ? [...expected] : [expected];
    if (expectedStates.length === 0 || expectedStates.some((state) => !isCoordinatorState(state))) {
      throw new SaasRegistryError("invalid expected coordinator state", "invalid");
    }
    if (lastError !== null && (/[\u0000-\u001f\u007f-\u009f]/u.test(lastError) || Buffer.byteLength(lastError, "utf8") > MAX_ERROR_BYTES)) {
      throw new SaasRegistryError("invalid redacted coordinator error", "invalid");
    }
    const timestamp = checkedNow(this.now);
    const stageColumn: Partial<Record<CoordinatorState, string>> = {
      seeded: "seeded_at_ms",
      running: "running_at_ms",
      routed: "routed_at_ms",
      invited: "invited_at_ms",
      active: "activated_at_ms",
      disabled: "disabled_at_ms",
      failed: "failed_at_ms",
    };
    const stage = stageColumn[next];
    const placeholders = expectedStates.map(() => "?").join(",");
    const assignments = ["state = ?", "updated_at_ms = ?", "last_error = ?"];
    if (stage) assignments.push(`${stage} = ?`);
    const bindings: Array<string | number | null> = [next, timestamp, lastError];
    if (stage) bindings.push(timestamp);
    bindings.push(id, ...expectedStates);
    const result = this.sqlite.query(
      `UPDATE coordinators SET ${assignments.join(", ")} WHERE id = ? AND state IN (${placeholders})`,
    ).run(...bindings);
    if (result.changes !== 1) {
      if (!this.sqlite.query("SELECT 1 FROM coordinators WHERE id = ?").get(id)) {
        throw new SaasRegistryError("coordinator not found", "not-found");
      }
      throw new SaasRegistryError("coordinator state changed concurrently", "conflict");
    }
    return this.getCoordinator(id);
  }

  setCoordinatorError(id: string, lastError: string | null): RegistryCoordinator {
    const current = this.getCoordinator(id);
    if (lastError !== null && (
      /[\u0000-\u001f\u007f-\u009f]/u.test(lastError)
      || Buffer.byteLength(lastError, "utf8") > MAX_ERROR_BYTES
    )) {
      throw new SaasRegistryError("invalid redacted coordinator error", "invalid");
    }
    const timestamp = checkedNow(this.now);
    const result = this.sqlite.query(`
      UPDATE coordinators SET updated_at_ms = ?, last_error = ?
      WHERE id = ? AND state = ?
    `).run(timestamp, lastError, id, current.state);
    if (result.changes !== 1) throw new SaasRegistryError("coordinator state changed concurrently", "conflict");
    return this.getCoordinator(id);
  }

  updateCoordinatorImageDigest(
    id: string,
    expectedDigest: string,
    nextDigest: string,
  ): RegistryCoordinator {
    assertCanonicalUuid(id, "coordinator id");
    assertImmutableImageDigest(expectedDigest);
    assertImmutableImageDigest(nextDigest);
    const timestamp = checkedNow(this.now);
    const result = this.sqlite.query(`
      UPDATE coordinators
      SET image_digest = ?, updated_at_ms = ?, last_error = NULL
      WHERE id = ? AND image_digest = ?
    `).run(nextDigest, timestamp, id, expectedDigest);
    if (result.changes !== 1) {
      throw new SaasRegistryError("coordinator image digest changed concurrently", "conflict");
    }
    return this.getCoordinator(id);
  }

  markActivationCommitted(accountId: string, coordinatorId: string): RegistryCoordinator {
    assertCanonicalUuid(accountId, "account id");
    assertCanonicalUuid(coordinatorId, "coordinator id");
    return immediate(this.sqlite, () => {
      const account = this.getAccount(accountId);
      const coordinator = this.getCoordinator(coordinatorId);
      if (coordinator.accountId !== accountId) throw new SaasRegistryError("activation registry identity mismatch", "corrupt");
      if (account.state === "active" && coordinator.state === "active") return coordinator;
      if (account.state !== "pending"
        || (coordinator.state !== "invited" && coordinator.state !== "routed")) {
        throw new SaasRegistryError("activation registry state is inconsistent", "conflict");
      }
      const timestamp = checkedNow(this.now);
      this.sqlite.query(
        "UPDATE accounts SET state = 'active', activated_at_ms = ?, disabled_at_ms = NULL WHERE id = ?",
      ).run(timestamp, accountId);
      this.sqlite.query(`
        UPDATE coordinators
        SET state = 'active', activated_at_ms = ?, updated_at_ms = ?, last_error = NULL
        WHERE id = ?
      `).run(timestamp, timestamp, coordinatorId);
      return this.getCoordinator(coordinatorId);
    });
  }

  restoreAccountAndCoordinator(
    accountId: string,
    coordinatorId: string,
    active: boolean,
  ): RegistryCoordinator {
    assertCanonicalUuid(accountId, "account id");
    assertCanonicalUuid(coordinatorId, "coordinator id");
    return immediate(this.sqlite, () => {
      const account = this.getAccount(accountId);
      const coordinator = this.getCoordinator(coordinatorId);
      const targetState: CoordinatorState = active ? "active" : "routed";
      const accountState: AccountState = active ? "active" : "pending";
      if (coordinator.accountId !== accountId) throw new SaasRegistryError("enable registry identity mismatch", "corrupt");
      if (account.state === accountState && coordinator.state === targetState) return coordinator;
      if (account.state !== "disabled" || coordinator.state !== "disabled") {
        throw new SaasRegistryError("enable registry state is inconsistent", "conflict");
      }
      const timestamp = checkedNow(this.now);
      if (active) {
        this.sqlite.query(`
          UPDATE accounts
          SET state = 'active', activated_at_ms = COALESCE(activated_at_ms, ?),
              disabled_at_ms = NULL
          WHERE id = ?
        `).run(timestamp, accountId);
        this.sqlite.query(`
          UPDATE coordinators
          SET state = 'active', activated_at_ms = COALESCE(activated_at_ms, ?),
              disabled_at_ms = NULL, updated_at_ms = ?, last_error = NULL
          WHERE id = ?
        `).run(timestamp, timestamp, coordinatorId);
      } else {
        this.sqlite.query(
          "UPDATE accounts SET state = 'pending', disabled_at_ms = NULL WHERE id = ?",
        ).run(accountId);
        this.sqlite.query(`
          UPDATE coordinators
          SET state = 'routed', routed_at_ms = ?, disabled_at_ms = NULL,
              updated_at_ms = ?, last_error = NULL
          WHERE id = ?
        `).run(timestamp, timestamp, coordinatorId);
      }
      return this.getCoordinator(coordinatorId);
    });
  }

  disableAccountAndCoordinator(accountId: string, coordinatorId: string): RegistryCoordinator {
    assertCanonicalUuid(accountId, "account id");
    assertCanonicalUuid(coordinatorId, "coordinator id");
    return immediate(this.sqlite, () => {
      const account = this.getAccount(accountId);
      const coordinator = this.getCoordinator(coordinatorId);
      if (coordinator.accountId !== accountId) throw new SaasRegistryError("disable registry identity mismatch", "corrupt");
      if (account.state === "disabled" && coordinator.state === "disabled") return coordinator;
      if (account.state === "disabled" || coordinator.state === "disabled") {
        throw new SaasRegistryError("disable registry state is inconsistent", "conflict");
      }
      const timestamp = checkedNow(this.now);
      this.sqlite.query(
        "UPDATE accounts SET state = 'disabled', disabled_at_ms = ? WHERE id = ?",
      ).run(timestamp, accountId);
      this.sqlite.query(`
        UPDATE coordinators
        SET state = 'disabled', disabled_at_ms = ?, updated_at_ms = ?, last_error = NULL
        WHERE id = ?
      `).run(timestamp, timestamp, coordinatorId);
      return this.getCoordinator(coordinatorId);
    });
  }

  markAccountActive(accountId: string): RegistryAccount {
    assertCanonicalUuid(accountId, "account id");
    const timestamp = checkedNow(this.now);
    const result = this.sqlite.query(`
      UPDATE accounts
      SET state = 'active', activated_at_ms = ?, disabled_at_ms = NULL
      WHERE id = ? AND state = 'pending'
    `).run(timestamp, accountId);
    if (result.changes !== 1) throw new SaasRegistryError("account is not pending", "conflict");
    return this.getAccount(accountId);
  }

  disableAccount(accountId: string): RegistryAccount {
    assertCanonicalUuid(accountId, "account id");
    const timestamp = checkedNow(this.now);
    const result = this.sqlite.query(`
      UPDATE accounts
      SET state = 'disabled', disabled_at_ms = ?
      WHERE id = ? AND state IN ('pending','active')
    `).run(timestamp, accountId);
    if (result.changes !== 1) throw new SaasRegistryError("account cannot be disabled", "conflict");
    return this.getAccount(accountId);
  }

  restoreAccount(accountId: string, active: boolean): RegistryAccount {
    assertCanonicalUuid(accountId, "account id");
    const next: AccountState = active ? "active" : "pending";
    const result = this.sqlite.query(`
      UPDATE accounts SET state = ?, disabled_at_ms = NULL
      WHERE id = ? AND state = 'disabled'
    `).run(next, accountId);
    if (result.changes !== 1) throw new SaasRegistryError("account is not disabled", "conflict");
    return this.getAccount(accountId);
  }
}
