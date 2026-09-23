// Approver-side pairing status read. The approving browser polls this after
// PairApprove so its code dialog can retire once the requester confirms.
// Called by handlers-pairing.ts::pairApprovalStatus; reads pair_requests only.
// Admission is exact-approver (or direct on-host for host approvals) and every
// other case is NotFound so the read never discloses foreign requests.

import { Code, ConnectError } from "@connectrpc/connect";
import type { KyselyDB } from "../db/connection.ts";

export type PairApprovalStatus =
  | "verification_required"
  | "completed"
  | "denied"
  | "expired"
  | "verification_failed";

export interface PairApprovalStatusInput {
  ephemeralId: string;
  /** Authenticated browser fingerprint, or null for a direct on-host caller. */
  callerFingerprint: string | null;
  onHost: boolean;
  now: number;
}

export async function readPairApprovalStatus(
  db: KyselyDB,
  input: PairApprovalStatusInput,
): Promise<PairApprovalStatus> {
  const row = await db.selectFrom("pair_requests")
    .select(["status", "approved_by_fp", "expires_at_ms"])
    .where("ephemeral_id", "=", input.ephemeralId)
    .executeTakeFirst();
  const admitted = row !== undefined && (
    row.approved_by_fp === null
      ? input.onHost
      : row.approved_by_fp === input.callerFingerprint
  );
  if (admitted) {
    switch (row.status) {
      case "verification_required":
        // Read-only normalization: the confirm path and retention sweep own
        // the durable expiry write.
        return row.expires_at_ms <= input.now ? "expired" : "verification_required";
      case "completed":
      case "denied":
      case "expired":
      case "verification_failed":
        return row.status;
    }
  }
  throw new ConnectError("not found", Code.NotFound);
}
