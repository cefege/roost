// Pair confirmation is the only path that turns a pending browser key into an
// account device. One transaction binds requester token, code, expiry,
// revocations, approver continuity, account ownership, and single-use state.
// The completed transition also returns the descriptive fields the
// "new browser paired" Sync notice carries (never secrets or provenance).

import { Code, ConnectError } from "@connectrpc/connect";
import { fingerprintOf } from "@roost/shared/fingerprint";
import type { KyselyDB } from "../db/connection.ts";
import {
  associatePairedBrowser,
  pairedBrowserAssociationConflict,
  pairingApprovalRemainsValid,
} from "./pairing-account.ts";
import {
  assertPairingCeremonyVersion,
  normalizePairRequestId,
  normalizePairRequesterToken,
  normalizePairVerificationCode,
  PAIRING_CLIENT_RELOAD_MESSAGE,
  PAIRING_CEREMONY_VERSION,
  PAIR_VERIFICATION_ATTEMPT_LIMIT,
  pairingSecretDigest,
} from "./pairing-secrets.ts";

export interface PairConfirmationInput {
  ceremonyVersion: number;
  ephemeralId: string;
  requesterToken: string;
  verificationCode: string;
}

export type PairConfirmationTerminalStatus = "expired" | "verification_failed";

/** Non-secret description of a browser that this confirmation just paired. */
export interface PairedBrowserNotice {
  ephemeralId: string;
  label: string;
  clientBrowser: string;
  clientOs: string;
  clientDeviceType: string;
  countryCode: string;
  region: string;
  city: string;
  pairedAtMs: number;
}

export interface PairConfirmationResult {
  ok: boolean;
  newlyAuthorizedFingerprint: string | null;
  /** Present only on the call whose transaction transitioned to completed. */
  pairedBrowser: PairedBrowserNotice | null;
  terminalStatus: PairConfirmationTerminalStatus | null;
}

export class PairConfirmationTerminalError extends ConnectError {
  constructor(
    message: string,
    code: Code,
    readonly terminalStatus: PairConfirmationTerminalStatus,
  ) {
    super(message, code);
  }

  // ConnectError's static hasInstance matches any ConnectError by name, which
  // subclasses inherit; without this every plain NotFound/FailedPrecondition
  // would be logged as a terminal transition with an undefined status.
  static override [Symbol.hasInstance](value: unknown): boolean {
    return value instanceof ConnectError && "terminalStatus" in value;
  }
}

type ConfirmationOutcome =
  | { kind: "completed"; fingerprint: string; pairedBrowser: PairedBrowserNotice }
  | { kind: "code_mismatch"; transitioned: boolean }
  | { kind: "expired" }
  | { kind: "authority_invalid" }
  | { kind: "not_live" };

export async function confirmPairRequest(
  db: KyselyDB,
  input: PairConfirmationInput,
): Promise<PairConfirmationResult> {
  assertPairingCeremonyVersion(input.ceremonyVersion);
  const ephemeralId = normalizePairRequestId(input.ephemeralId);
  if (ephemeralId === null) {
    throw new ConnectError("invalid pair request id", Code.InvalidArgument);
  }
  const requesterToken = normalizePairRequesterToken(input.requesterToken);
  if (requesterToken === null) {
    throw new ConnectError("invalid pair requester token", Code.InvalidArgument);
  }
  const verificationCode = normalizePairVerificationCode(input.verificationCode);
  if (verificationCode === null) {
    throw new ConnectError(
      "verification code must contain exactly six ASCII digits",
      Code.InvalidArgument,
    );
  }
  const [requesterTokenHash, verificationCodeHash] = await Promise.all([
    pairingSecretDigest(requesterToken),
    pairingSecretDigest(verificationCode),
  ]);
  const now = Date.now();

  const outcome = await db.transaction().execute(async (trx): Promise<ConfirmationOutcome> => {
    const row = await trx.selectFrom("pair_requests")
      .selectAll()
      .where("ephemeral_id", "=", ephemeralId)
      .where("requester_token_hash", "=", requesterTokenHash)
      .where("requester_token_hash", "!=", "")
      .executeTakeFirst();
    if (!row) throw new ConnectError("not found", Code.NotFound);
    if (row.ceremony_version !== PAIRING_CEREMONY_VERSION) {
      throw new ConnectError(PAIRING_CLIENT_RELOAD_MESSAGE, Code.FailedPrecondition);
    }
    if (row.status !== "verification_required") return { kind: "not_live" };

    const failVerification = async (): Promise<void> => {
      await trx.updateTable("pair_requests")
        .set({
          status: "verification_failed",
          decided_at_ms: now,
          verification_code_hash: null,
        })
        .where("id", "=", row.id)
        .where("status", "=", "verification_required")
        .executeTakeFirstOrThrow();
    };

    if (row.expires_at_ms <= now) {
      await trx.updateTable("pair_requests")
        .set({
          status: "expired",
          decided_at_ms: now,
          verification_code_hash: null,
        })
        .where("id", "=", row.id)
        .where("status", "=", "verification_required")
        .executeTakeFirstOrThrow();
      return { kind: "expired" };
    }

    const publicKey = row.public_key instanceof Uint8Array
      ? row.public_key
      : new Uint8Array(row.public_key);
    const fingerprint = await fingerprintOf(publicKey);
    const requesterRevocation = await trx.selectFrom("authorized_key_revocations")
      .select("fingerprint")
      .where("fingerprint", "=", fingerprint)
      .executeTakeFirst();
    const approvalValid = await pairingApprovalRemainsValid(
      trx,
      row.approved_by_fp,
      row.approved_account_id,
    );
    if (requesterRevocation || !approvalValid) {
      await failVerification();
      return { kind: "authority_invalid" };
    }

    if (
      row.verification_attempts >= PAIR_VERIFICATION_ATTEMPT_LIMIT
      || row.verification_code_hash === null
      || row.verification_code_hash !== verificationCodeHash
    ) {
      const attempts = Math.min(
        PAIR_VERIFICATION_ATTEMPT_LIMIT,
        row.verification_attempts + 1,
      );
      const exhausted = attempts >= PAIR_VERIFICATION_ATTEMPT_LIMIT
        || row.verification_code_hash === null;
      await trx.updateTable("pair_requests")
        .set({
          verification_attempts: attempts,
          status: exhausted ? "verification_failed" : "verification_required",
          decided_at_ms: exhausted ? now : null,
          verification_code_hash: exhausted ? null : row.verification_code_hash,
        })
        .where("id", "=", row.id)
        .where("status", "=", "verification_required")
        .executeTakeFirstOrThrow();
      return { kind: "code_mismatch", transitioned: exhausted };
    }

    const accountId = row.approved_account_id;
    if (accountId === null) {
      await failVerification();
      return { kind: "authority_invalid" };
    }
    const associationConflict = await pairedBrowserAssociationConflict(
      trx,
      fingerprint,
      accountId,
    );
    if (associationConflict !== null) {
      await failVerification();
      return { kind: "authority_invalid" };
    }
    await trx.insertInto("authorized_keys").values({
      fingerprint,
      public_key: publicKey,
      label: row.label,
      added_at: now,
      paired_from_ip: row.source_ip,
      paired_country: row.country_code,
      paired_user_agent: row.user_agent,
      paired_edge_identity: row.edge_identity,
    }).onConflict((conflict) => conflict.column("fingerprint").doUpdateSet({
      label: row.label,
      paired_from_ip: row.source_ip,
      paired_country: row.country_code,
      paired_user_agent: row.user_agent,
      paired_edge_identity: row.edge_identity,
    })).execute();
    await associatePairedBrowser(trx, fingerprint, accountId, now);
    await trx.updateTable("pair_requests")
      .set({
        status: "completed",
        decided_at_ms: now,
        verification_code_hash: null,
      })
      .where("id", "=", row.id)
      .where("status", "=", "verification_required")
      .executeTakeFirstOrThrow();
    return {
      kind: "completed",
      fingerprint,
      pairedBrowser: {
        ephemeralId,
        label: row.label,
        clientBrowser: row.client_browser ?? "",
        clientOs: row.client_os ?? "",
        clientDeviceType: row.client_device_type ?? "",
        countryCode: row.country_code ?? "",
        region: row.region ?? "",
        city: row.city ?? "",
        pairedAtMs: now,
      },
    };
  });

  switch (outcome.kind) {
    case "completed":
      return {
        ok: true,
        newlyAuthorizedFingerprint: outcome.fingerprint,
        pairedBrowser: outcome.pairedBrowser,
        terminalStatus: null,
      };
    case "code_mismatch":
      return {
        ok: false,
        newlyAuthorizedFingerprint: null,
        pairedBrowser: null,
        terminalStatus: outcome.transitioned ? "verification_failed" : null,
      };
    case "expired":
      throw new PairConfirmationTerminalError(
        "pair request expired",
        Code.FailedPrecondition,
        "expired",
      );
    case "authority_invalid":
      throw new PairConfirmationTerminalError(
        "pairing authority is no longer valid",
        Code.PermissionDenied,
        "verification_failed",
      );
    case "not_live":
      throw new ConnectError("pair request is not awaiting verification", Code.FailedPrecondition);
  }
}
