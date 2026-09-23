// Pairing handlers validate the browser wire ceremony and publish committed
// request-state deltas. Durable request/approval transitions and confirmation
// authority remain isolated in their dedicated pairing owners.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import { log } from "@roost/shared/log";
import {
  CoordinatorService,
  PairApproveResponseSchema,
  PairConfirmResponseSchema,
  PairCreateResponseSchema,
  PairDenyResponseSchema,
  PairListResponseSchema,
  PairPollResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { PairRequestSchema } from "@roost/shared/proto/wire_pb";
import { decodeEd25519Pubkey } from "../authorized-keys.ts";
import { pairBus } from "../buses.ts";
import { refreshJwtKey } from "../jwt.ts";
import { assertOnHost } from "../middleware/caller-origin.ts";
import { writeAuditLog } from "../middleware/security.ts";
import {
  callerOrigin,
  optionalAccountDevice,
} from "./auth-interceptor.ts";
import { capturePairRequestProvenance } from "./pair-request-provenance.ts";
import {
  approvePairRequest,
  createPairRequest,
} from "./pairing-account.ts";
import {
  confirmPairRequest,
  PairConfirmationTerminalError,
  type PairConfirmationResult,
} from "./pairing-confirmation.ts";
import {
  assertPairingCeremonyVersion,
  normalizePairRequestId,
  normalizePairRequesterToken,
  normalizePairVerificationCode,
  PAIRING_CLIENT_RELOAD_MESSAGE,
  PAIRING_CEREMONY_VERSION,
  pairingSecretDigest,
} from "./pairing-secrets.ts";
import type { ConnectDeps } from "./router.ts";

export const PAIR_REQUEST_TTL_MS = 10 * 60_000;

type PairingMethods =
  | "pairCreate"
  | "pairPoll"
  | "pairList"
  | "pairApprove"
  | "pairConfirm"
  | "pairDeny";

export function makePairingHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, PairingMethods> {
  return {
    async pairCreate(req, ctx) {
      assertPairingCeremonyVersion(req.ceremonyVersion);
      const ephemeralId = normalizePairRequestId(req.ephemeralId);
      if (ephemeralId === null) {
        throw new ConnectError("invalid pair request id", Code.InvalidArgument);
      }
      const requesterToken = normalizePairRequesterToken(req.requesterToken);
      if (requesterToken === null) {
        throw new ConnectError("invalid pair requester token", Code.InvalidArgument);
      }
      const publicKey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!publicKey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const requesterTokenHash = await pairingSecretDigest(requesterToken);
      const origin = callerOrigin(ctx.values);
      const requestHeaders = ctx.requestHeader ?? new Headers();
      let accessIdentity: { email: string } | null = null;
      if (deps.cfAccess && !origin.onHost) {
        accessIdentity = await deps.cfAccess.verify(requestHeaders);
        if (accessIdentity === null) {
          throw new ConnectError("pairing requires front-door sign-in", Code.Unauthenticated);
        }
      }
      const now = Date.now();
      const expiresAtMs = now + PAIR_REQUEST_TTL_MS;
      const provenance = capturePairRequestProvenance(requestHeaders, origin);
      const edgeIdentityProvider = accessIdentity === null ? null : "cloudflare-access";
      const edgeIdentity = accessIdentity?.email ?? null;
      const edgeIdentityVerified = accessIdentity === null ? 0 : 1;
      const creation = await createPairRequest(deps.db, {
        ephemeralId,
        requesterTokenHash,
        publicKey,
        label: req.label,
        now,
        expiresAtMs,
        provenance,
        edgeIdentityProvider,
        edgeIdentity,
        edgeIdentityVerified,
      });
      for (const expiredId of creation.expiredIds) {
        pairBus.publish({ kind: "removed", ephemeral_id: expiredId });
      }
      if (creation.kind === "expired") {
        throw new ConnectError("pair request expired", Code.FailedPrecondition);
      }
      if (creation.kind === "retry") {
        return create(PairCreateResponseSchema, { ephemeralId });
      }
      pairBus.publish({
        kind: "pending",
        ephemeral_id: ephemeralId,
        label: req.label,
        created_at_ms: now,
        user_agent: provenance.userAgent ?? "",
        client_browser: provenance.clientBrowser ?? "",
        client_os: provenance.clientOs ?? "",
        client_device_type: provenance.clientDeviceType ?? "",
        source_ip: provenance.sourceIp,
        country_code: provenance.countryCode ?? "",
        region: provenance.region ?? "",
        city: provenance.city ?? "",
        edge_identity_provider: edgeIdentityProvider ?? "",
        edge_identity: edgeIdentity ?? "",
        edge_identity_verified: Boolean(edgeIdentityVerified),
        expires_at_ms: expiresAtMs,
      });
      log.info("pair.connect", "created", {
        ephemeral_id: ephemeralId,
        label: req.label,
        client_ip: provenance.sourceIp,
        country_code: provenance.countryCode,
        edge_identity_verified: edgeIdentityVerified,
      });
      return create(PairCreateResponseSchema, { ephemeralId });
    },

    async pairPoll(req, _ctx) {
      assertPairingCeremonyVersion(req.ceremonyVersion);
      const ephemeralId = normalizePairRequestId(req.ephemeralId);
      if (ephemeralId === null) {
        throw new ConnectError("invalid pair request id", Code.InvalidArgument);
      }
      const requesterToken = normalizePairRequesterToken(req.requesterToken);
      if (requesterToken === null) {
        throw new ConnectError("invalid pair requester token", Code.InvalidArgument);
      }
      const requesterTokenHash = await pairingSecretDigest(requesterToken);
      const row = await deps.db.selectFrom("pair_requests")
        .select(["ceremony_version", "expires_at_ms", "status"])
        .where("ephemeral_id", "=", ephemeralId)
        .where("requester_token_hash", "=", requesterTokenHash)
        .where("requester_token_hash", "!=", "")
        .executeTakeFirst();
      if (!row) throw new ConnectError("not found", Code.NotFound);
      if (row.ceremony_version !== PAIRING_CEREMONY_VERSION) {
        throw new ConnectError(PAIRING_CLIENT_RELOAD_MESSAGE, Code.FailedPrecondition);
      }
      const live = row.status === "pending" || row.status === "verification_required";
      const status = live && row.expires_at_ms <= Date.now() ? "expired" : row.status;
      switch (status) {
        case "pending":
        case "verification_required":
        case "denied":
        case "expired":
        case "verification_failed":
        case "completed":
          return create(PairPollResponseSchema, {
            status,
            expiresAtMs: BigInt(row.expires_at_ms),
          });
        default:
          throw new ConnectError("not found", Code.NotFound);
      }
    },

    async pairList(_req, ctx) {
      if (!optionalAccountDevice(ctx.values)) assertOnHost(callerOrigin(ctx.values));
      const rows = await deps.db.selectFrom("pair_requests")
        .select([
          "ephemeral_id",
          "label",
          "created_at_ms",
          "user_agent",
          "client_browser",
          "client_os",
          "client_device_type",
          "source_ip",
          "country_code",
          "region",
          "city",
          "edge_identity_provider",
          "edge_identity",
          "edge_identity_verified",
          "expires_at_ms",
        ])
        .where("status", "=", "pending")
        .where("expires_at_ms", ">", Date.now())
        .orderBy("created_at_ms", "desc")
        .execute();
      return create(PairListResponseSchema, {
        requests: rows.map((row) => create(PairRequestSchema, {
          ephemeralId: row.ephemeral_id,
          label: row.label,
          createdAtMs: BigInt(row.created_at_ms),
          userAgent: row.user_agent ?? "",
          clientBrowser: row.client_browser ?? "",
          clientOs: row.client_os ?? "",
          clientDeviceType: row.client_device_type ?? "",
          sourceIp: row.source_ip ?? "",
          countryCode: row.country_code ?? "",
          region: row.region ?? "",
          city: row.city ?? "",
          edgeIdentityProvider: row.edge_identity_provider ?? "",
          edgeIdentity: row.edge_identity ?? "",
          edgeIdentityVerified: row.edge_identity_verified === 1,
          expiresAtMs: BigInt(row.expires_at_ms),
        })),
      });
    },

    async pairApprove(req, ctx) {
      assertPairingCeremonyVersion(req.ceremonyVersion);
      const ephemeralId = normalizePairRequestId(req.ephemeralId);
      if (ephemeralId === null) {
        throw new ConnectError("invalid pair request id", Code.InvalidArgument);
      }
      const verificationCode = normalizePairVerificationCode(req.verificationCode);
      if (verificationCode === null) {
        throw new ConnectError(
          "verification code must contain exactly six ASCII digits",
          Code.InvalidArgument,
        );
      }
      const verificationCodeHash = await pairingSecretDigest(verificationCode);
      const caller = optionalAccountDevice(ctx.values);
      if (!caller) assertOnHost(callerOrigin(ctx.values));
      const approval = await approvePairRequest(deps.db, {
        ephemeralId,
        verificationCodeHash,
        approverFingerprint: caller?.fingerprint ?? null,
        now: Date.now(),
      });
      if (approval.kind === "expired") {
        pairBus.publish({ kind: "removed", ephemeral_id: ephemeralId });
        log.info("pair.connect", "expired", { ephemeral_id: ephemeralId });
        throw new ConnectError("pair request expired", Code.FailedPrecondition);
      }
      if (approval.kind === "approved") {
        pairBus.publish({ kind: "removed", ephemeral_id: ephemeralId });
        log.info("pair.connect", "verification_required", {
          ephemeral_id: ephemeralId,
          fp: approval.requesterFingerprint,
        });
      }
      return create(PairApproveResponseSchema, { ok: true });
    },

    async pairConfirm(req, _ctx) {
      assertPairingCeremonyVersion(req.ceremonyVersion);
      let result: PairConfirmationResult;
      try {
        result = await confirmPairRequest(deps.db, {
          ceremonyVersion: req.ceremonyVersion,
          ephemeralId: req.ephemeralId,
          requesterToken: req.requesterToken,
          verificationCode: req.verificationCode,
        });
      } catch (error) {
        if (error instanceof PairConfirmationTerminalError) {
          log.info("pair.connect", error.terminalStatus, {
            ephemeral_id: req.ephemeralId,
          });
        }
        throw error;
      }
      if (result.terminalStatus !== null) {
        log.info("pair.connect", result.terminalStatus, {
          ephemeral_id: req.ephemeralId,
        });
      }
      if (result.newlyAuthorizedFingerprint !== null) {
        refreshJwtKey(deps.jwtCache, result.newlyAuthorizedFingerprint);
        await writeAuditLog({
          db: deps.db,
          callerFp: result.newlyAuthorizedFingerprint,
          dashboardId: deps.selfHostedTenant.dashboardId,
          method: "POST",
          path: "/roost.v1.CoordinatorService/PairConfirm",
          recordTelemetry: false,
          status: 200,
          traceId: undefined,
        });
        log.info("pair.connect", "completed", {
          ephemeral_id: req.ephemeralId,
          fp: result.newlyAuthorizedFingerprint,
        });
      }
      return create(PairConfirmResponseSchema, { ok: result.ok });
    },

    async pairDeny(req, ctx) {
      const ephemeralId = normalizePairRequestId(req.ephemeralId);
      if (ephemeralId === null) {
        throw new ConnectError("invalid pair request id", Code.InvalidArgument);
      }
      if (!optionalAccountDevice(ctx.values)) assertOnHost(callerOrigin(ctx.values));
      const result = await deps.db.updateTable("pair_requests")
        .set({ status: "denied", decided_at_ms: Date.now(), verification_code_hash: null })
        .where("ephemeral_id", "=", ephemeralId)
        .where("status", "in", ["pending", "verification_required"])
        .returningAll().executeTakeFirst();
      if (!result) throw new ConnectError("not found", Code.NotFound);
      pairBus.publish({ kind: "removed", ephemeral_id: ephemeralId });
      log.info("pair.connect", "denied", { ephemeral_id: ephemeralId });
      return create(PairDenyResponseSchema, { ok: true });
    },
  };
}
