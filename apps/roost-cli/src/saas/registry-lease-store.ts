// Owns coordinator-scoped and global operation leases in the SaaS registry.
// Lifecycle and provisioning workers use these compare-and-set lease records.
// Immediate acquisition prevents concurrent owners from crossing side-effect boundaries.
import { RegistryLinkTicketStore } from "./registry-link-ticket-store.ts";
import { immediate } from "./registry-schema.ts";
import type { RegistryGlobalLease, RegistryLease } from "./registry-model.ts";
import type { RawGlobalLease, RawLease } from "./registry-row-types.ts";
import { mapGlobalLease, mapLease } from "./registry-row-mappers.ts";
import {
  SAFE_LEASE_VALUE_RE,
  SaasRegistryError,
  assertCanonicalUuid,
  checkedNow,
} from "./registry-validation.ts";

export class RegistryLeaseStore extends RegistryLinkTicketStore {
  acquireLease(coordinatorId: string, operation: string, owner: string, ttlMs: number): RegistryLease {
    assertCanonicalUuid(coordinatorId, "coordinator id");
    if (!SAFE_LEASE_VALUE_RE.test(operation) || !SAFE_LEASE_VALUE_RE.test(owner)) {
      throw new SaasRegistryError("invalid lease identity", "invalid");
    }
    if (!Number.isSafeInteger(ttlMs) || ttlMs < 1 || ttlMs > 24 * 60 * 60 * 1000) {
      throw new SaasRegistryError("invalid lease duration", "invalid");
    }
    return immediate(this.sqlite, () => {
      if (!this.sqlite.query("SELECT 1 FROM coordinators WHERE id = ?").get(coordinatorId)) {
        throw new SaasRegistryError("coordinator not found", "not-found");
      }
      const timestamp = checkedNow(this.now);
      const expiresAtMs = timestamp + ttlMs;
      if (!Number.isSafeInteger(expiresAtMs)) throw new SaasRegistryError("lease expiry overflow", "invalid");
      const existing = this.sqlite.query(
        "SELECT * FROM operation_leases WHERE coordinator_id = ?",
      ).get(coordinatorId) as RawLease | null;
      if (existing && existing.expires_at_ms > timestamp && (existing.owner !== owner || existing.operation !== operation)) {
        throw new SaasRegistryError("coordinator operation lease is held", "lease-held");
      }
      this.sqlite.query(`
        INSERT INTO operation_leases (coordinator_id, operation, owner, acquired_at_ms, expires_at_ms)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(coordinator_id) DO UPDATE SET
          operation = excluded.operation,
          owner = excluded.owner,
          acquired_at_ms = excluded.acquired_at_ms,
          expires_at_ms = excluded.expires_at_ms
      `).run(coordinatorId, operation, owner, timestamp, expiresAtMs);
      return this.getLease(coordinatorId)!;
    });
  }

  getLease(coordinatorId: string): RegistryLease | null {
    assertCanonicalUuid(coordinatorId, "coordinator id");
    const raw = this.sqlite.query(
      "SELECT * FROM operation_leases WHERE coordinator_id = ?",
    ).get(coordinatorId) as RawLease | null;
    return raw ? mapLease(raw) : null;
  }

  renewLease(coordinatorId: string, operation: string, owner: string, ttlMs: number): RegistryLease {
    assertCanonicalUuid(coordinatorId, "coordinator id");
    if (!SAFE_LEASE_VALUE_RE.test(operation) || !SAFE_LEASE_VALUE_RE.test(owner)) {
      throw new SaasRegistryError("invalid lease identity", "invalid");
    }
    if (!Number.isSafeInteger(ttlMs) || ttlMs < 1 || ttlMs > 24 * 60 * 60 * 1000) {
      throw new SaasRegistryError("invalid lease duration", "invalid");
    }
    const timestamp = checkedNow(this.now);
    const expiresAtMs = timestamp + ttlMs;
    const result = this.sqlite.query(`
      UPDATE operation_leases SET expires_at_ms = ?
      WHERE coordinator_id = ? AND operation = ? AND owner = ? AND expires_at_ms > ?
    `).run(expiresAtMs, coordinatorId, operation, owner, timestamp);
    if (result.changes !== 1) throw new SaasRegistryError("operation lease was lost", "lease-held");
    return this.getLease(coordinatorId)!;
  }

  releaseLease(coordinatorId: string, operation: string, owner: string): void {
    assertCanonicalUuid(coordinatorId, "coordinator id");
    const result = this.sqlite.query(`
      DELETE FROM operation_leases WHERE coordinator_id = ? AND operation = ? AND owner = ?
    `).run(coordinatorId, operation, owner);
    if (result.changes !== 1) throw new SaasRegistryError("operation lease was lost", "lease-held");
  }

  acquireGlobalLease(resource: string, operation: string, owner: string, ttlMs: number): RegistryGlobalLease {
    if (!SAFE_LEASE_VALUE_RE.test(resource)
      || !SAFE_LEASE_VALUE_RE.test(operation)
      || !SAFE_LEASE_VALUE_RE.test(owner)) {
      throw new SaasRegistryError("invalid global lease identity", "invalid");
    }
    if (!Number.isSafeInteger(ttlMs) || ttlMs < 1 || ttlMs > 24 * 60 * 60 * 1000) {
      throw new SaasRegistryError("invalid global lease duration", "invalid");
    }
    return immediate(this.sqlite, () => {
      const timestamp = checkedNow(this.now);
      const expiresAtMs = timestamp + ttlMs;
      const existing = this.sqlite.query(
        "SELECT * FROM global_leases WHERE resource = ?",
      ).get(resource) as RawGlobalLease | null;
      if (existing && existing.expires_at_ms > timestamp
        && (existing.owner !== owner || existing.operation !== operation)) {
        throw new SaasRegistryError("global operation lease is held", "lease-held");
      }
      this.sqlite.query(`
        INSERT INTO global_leases (resource, operation, owner, acquired_at_ms, expires_at_ms)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(resource) DO UPDATE SET
          operation = excluded.operation,
          owner = excluded.owner,
          acquired_at_ms = excluded.acquired_at_ms,
          expires_at_ms = excluded.expires_at_ms
      `).run(resource, operation, owner, timestamp, expiresAtMs);
      const row = this.sqlite.query("SELECT * FROM global_leases WHERE resource = ?")
        .get(resource) as RawGlobalLease;
      return mapGlobalLease(row);
    });
  }

  getGlobalLease(resource: string): RegistryGlobalLease | null {
    if (!SAFE_LEASE_VALUE_RE.test(resource)) {
      throw new SaasRegistryError("invalid global lease identity", "invalid");
    }
    const row = this.sqlite.query("SELECT * FROM global_leases WHERE resource = ?")
      .get(resource) as RawGlobalLease | null;
    return row ? mapGlobalLease(row) : null;
  }

  renewGlobalLease(
    resource: string,
    operation: string,
    owner: string,
    ttlMs: number,
  ): RegistryGlobalLease {
    if (!Number.isSafeInteger(ttlMs) || ttlMs < 1 || ttlMs > 24 * 60 * 60 * 1000) {
      throw new SaasRegistryError("invalid global lease duration", "invalid");
    }
    const timestamp = checkedNow(this.now);
    const expiresAtMs = timestamp + ttlMs;
    const result = this.sqlite.query(`
      UPDATE global_leases SET expires_at_ms = ?
      WHERE resource = ? AND operation = ? AND owner = ? AND expires_at_ms > ?
    `).run(expiresAtMs, resource, operation, owner, timestamp);
    if (result.changes !== 1) throw new SaasRegistryError("global operation lease was lost", "lease-held");
    const row = this.sqlite.query("SELECT * FROM global_leases WHERE resource = ?")
      .get(resource) as RawGlobalLease;
    return mapGlobalLease(row);
  }

  releaseGlobalLease(resource: string, operation: string, owner: string): void {
    const result = this.sqlite.query(
      "DELETE FROM global_leases WHERE resource = ? AND operation = ? AND owner = ?",
    ).run(resource, operation, owner);
    if (result.changes !== 1) throw new SaasRegistryError("global operation lease was lost", "lease-held");
  }
}
