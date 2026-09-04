// Owns the Kysely implementation of the durable email outbox storage seam.
// The public outbox module exposes it for coordinator startup after migrations.
// It depends only on the coordinator schema, Kysely, and opaque UUID leases.
// Atomic claims and ID-plus-token CAS updates prevent overlapping ownership.

import { randomUUID } from "node:crypto";
import { sql, type Kysely } from "kysely";
import type { DB } from "./db/schema.ts";
import type { EmailOutboxLease, EmailOutboxStore } from "./email-outbox.ts";

/**
 * Kysely-backed storage for the migrated email_outbox table. Claiming is one
 * UPDATE ... WHERE id IN (due subquery) ... RETURNING statement, so competing
 * dispatchers cannot both receive one row. Completion mutations CAS on the
 * lease token, preventing an expired owner from overwriting a later lease.
 */
export function createKyselyEmailOutboxStore(db: Kysely<DB>): EmailOutboxStore {
  return {
    async claimDue({ nowMs, leaseDurationMs, limit }): Promise<EmailOutboxLease[]> {
      const lockedUntilMs = nowMs + leaseDurationMs;
      const leaseToken = randomUUID();
      const dueIds = db.selectFrom("email_outbox")
        .select("id")
        .where((eb) => eb.or([
          eb.and([
            eb("state", "=", "pending"),
            eb("next_attempt_ms", "<=", nowMs),
          ]),
          eb.and([
            eb("state", "=", "sending"),
            eb("locked_until_ms", "is not", null),
            eb("locked_until_ms", "<=", nowMs),
          ]),
        ]))
        .orderBy("next_attempt_ms")
        .orderBy("id")
        .limit(limit);

      const rows = await db.updateTable("email_outbox")
        .set({
          state: "sending",
          locked_until_ms: lockedUntilMs,
          lease_token: leaseToken,
          attempts: sql<number>`attempts + 1`,
        })
        .where("id", "in", dueIds)
        .returning([
          "id",
          "kind",
          "recipient",
          "encrypted_payload",
          "attempts",
          "lease_token",
        ])
        .execute();

      return rows.map((row) => {
        if (!row.lease_token) throw new Error("email outbox lease token missing");
        return {
          id: row.id,
          kind: row.kind,
          recipient: row.recipient,
          encryptedPayload: row.encrypted_payload,
          attempt: row.attempts,
          leaseToken: row.lease_token,
        };
      });
    },

    async markSent(lease, providerMessageId, nowMs): Promise<boolean> {
      const updated = await db.updateTable("email_outbox")
        .set({
          state: "sent",
          locked_until_ms: null,
          lease_token: null,
          provider_message_id: providerMessageId,
          sent_at_ms: nowMs,
          failed_at_ms: null,
          last_error: null,
        })
        .where("id", "=", lease.id)
        .where("state", "=", "sending")
        .where("lease_token", "=", lease.leaseToken)
        .returning("id")
        .executeTakeFirst();
      return updated !== undefined;
    },

    async reschedule(lease, update): Promise<boolean> {
      const updated = await db.updateTable("email_outbox")
        .set({
          state: "pending",
          locked_until_ms: null,
          lease_token: null,
          next_attempt_ms: update.nextAttemptMs,
          last_error: update.reason,
        })
        .where("id", "=", lease.id)
        .where("state", "=", "sending")
        .where("lease_token", "=", lease.leaseToken)
        .returning("id")
        .executeTakeFirst();
      return updated !== undefined;
    },

    async markFailed(lease, update): Promise<boolean> {
      const updated = await db.updateTable("email_outbox")
        .set({
          state: "failed",
          locked_until_ms: null,
          lease_token: null,
          failed_at_ms: update.nowMs,
          last_error: update.reason,
        })
        .where("id", "=", lease.id)
        .where("state", "=", "sending")
        .where("lease_token", "=", lease.leaseToken)
        .returning("id")
        .executeTakeFirst();
      return updated !== undefined;
    },
  };
}
