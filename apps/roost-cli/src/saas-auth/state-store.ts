/**
 * Exposes the signup gateway's durable state API while delegating storage by domain.
 * Auth protocols keep one GatewayStateStore so every operation shares a database and cipher.
 * The facade preserves the established API while focused modules own each state transition.
 */

import {
  beginGatewayTurnstileVerification,
  consumeGatewayRateBucket,
  markGatewayTurnstileFailed,
  markGatewayTurnstileVerified,
} from "./state-store-abuse.ts";
import { openGatewayStateContext } from "./state-store-database.ts";
import type { GatewayStateContext } from "./state-store-database.ts";
import {
  claimDueGatewaySignupEmails,
  consumeGatewayEmailChallenge,
  createGatewayEmailChallenge,
  failGatewaySignupEmail,
  markGatewaySignupEmailSent,
  rescheduleGatewaySignupEmail,
  verifyGatewayEmailChallenge,
} from "./state-store-email.ts";
import {
  consumeGatewayOAuthAttempt,
  startGatewayOAuthAttempt,
} from "./state-store-oauth.ts";
import {
  bindGatewayResultAssertion,
  createGatewayResultReceipt,
  createGatewayResultReceiptForBrowserHash,
  getGatewayAssertionInput,
  getGatewayResult,
  setGatewayResultOutcome,
} from "./state-store-results.ts";
import type {
  ClaimSignupEmailOptions,
  ConsumedOAuthAttempt,
  ConsumeRateBucketOptions,
  CreateEmailChallengeOptions,
  CreateHashedResultReceiptOptions,
  CreateResultReceiptOptions,
  GatewayEmailChallenge,
  GatewayResultReceipt,
  OpenGatewayStateStoreOptions,
  OAuthAttemptInput,
  RateBucketResult,
  SetResultOutcomeOptions,
  SignupEmailLease,
  TurnstileVerification,
} from "./state-store-types.ts";

export * from "./state-store-types.ts";

export class GatewayStateStore {
  readonly path: string;
  private readonly context: GatewayStateContext;

  constructor(options: OpenGatewayStateStoreOptions) {
    this.context = openGatewayStateContext(options);
    this.path = this.context.path;
  }

  close(): void {
    if (this.context.sqlite.inTransaction) this.context.sqlite.exec("ROLLBACK");
    this.context.sqlite.close();
  }

  consumeRateBucket(options: ConsumeRateBucketOptions): RateBucketResult {
    return consumeGatewayRateBucket(this.context, options);
  }

  beginTurnstileVerification(
    tokenHashRaw: string,
    idempotencyKeyRaw: string,
    nowMsRaw = this.context.now(),
  ): TurnstileVerification {
    return beginGatewayTurnstileVerification(this.context, tokenHashRaw, idempotencyKeyRaw, nowMsRaw);
  }

  markTurnstileVerified(
    tokenHashRaw: string,
    idempotencyKeyRaw: string,
    nowMsRaw = this.context.now(),
  ): boolean {
    return markGatewayTurnstileVerified(this.context, tokenHashRaw, idempotencyKeyRaw, nowMsRaw);
  }

  markTurnstileFailed(tokenHashRaw: string, idempotencyKeyRaw: string): boolean {
    return markGatewayTurnstileFailed(this.context, tokenHashRaw, idempotencyKeyRaw);
  }

  createEmailChallenge(options: CreateEmailChallengeOptions): GatewayEmailChallenge {
    return createGatewayEmailChallenge(this.context, options);
  }

  verifyEmailChallenge(
    tokenHashRaw: string,
    nowMsRaw = this.context.now(),
  ): GatewayEmailChallenge | null {
    return verifyGatewayEmailChallenge(this.context, tokenHashRaw, nowMsRaw);
  }

  consumeEmailChallenge(idRaw: string, nowMsRaw = this.context.now()): boolean {
    return consumeGatewayEmailChallenge(this.context, idRaw, nowMsRaw);
  }

  claimDueSignupEmails(options: ClaimSignupEmailOptions): SignupEmailLease[] {
    return claimDueGatewaySignupEmails(this.context, options);
  }

  markSignupEmailSent(
    lease: SignupEmailLease,
    providerMessageIdRaw: string,
    nowMsRaw = this.context.now(),
  ): boolean {
    return markGatewaySignupEmailSent(this.context, lease, providerMessageIdRaw, nowMsRaw);
  }

  rescheduleSignupEmail(
    lease: SignupEmailLease,
    nextAttemptMsRaw: number,
    reasonRaw: string,
  ): boolean {
    return rescheduleGatewaySignupEmail(this.context, lease, nextAttemptMsRaw, reasonRaw);
  }

  failSignupEmail(
    lease: SignupEmailLease,
    reasonRaw: string,
    nowMsRaw = this.context.now(),
  ): boolean {
    return failGatewaySignupEmail(this.context, lease, reasonRaw, nowMsRaw);
  }

  startOAuthAttempt(input: OAuthAttemptInput): string {
    return startGatewayOAuthAttempt(this.context, input);
  }

  consumeOAuthAttempt(
    oauthCookieRaw: string,
    stateRaw: string,
    nowMsRaw = this.context.now(),
  ): ConsumedOAuthAttempt | null {
    return consumeGatewayOAuthAttempt(this.context, oauthCookieRaw, stateRaw, nowMsRaw);
  }

  createResultReceipt(options: CreateResultReceiptOptions): void {
    createGatewayResultReceipt(this.context, options);
  }

  createResultReceiptForBrowserHash(options: CreateHashedResultReceiptOptions): void {
    createGatewayResultReceiptForBrowserHash(this.context, options);
  }

  setResultOutcome(options: SetResultOutcomeOptions): boolean {
    return setGatewayResultOutcome(this.context, options);
  }

  getAssertionInput(receiptRaw: string, nowMsRaw = this.context.now()): string | null {
    return getGatewayAssertionInput(this.context, receiptRaw, nowMsRaw);
  }

  bindResultAssertion(
    receiptRaw: string,
    fingerprintRaw: string,
    assertionRaw: string,
    nowMsRaw = this.context.now(),
  ): string | null {
    return bindGatewayResultAssertion(
      this.context,
      receiptRaw,
      fingerprintRaw,
      assertionRaw,
      nowMsRaw,
    );
  }

  getResult(receiptRaw: string, nowMsRaw = this.context.now()): GatewayResultReceipt | null {
    return getGatewayResult(this.context, receiptRaw, nowMsRaw);
  }
}
