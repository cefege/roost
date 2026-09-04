// Narrow coordinator composition boundary for SaaS email delivery. Main creates
// this once after migrations and owns only start/stop; request handlers receive
// its encryption method to transactionally insert their own outbox rows.

import type { Kysely } from "kysely";
import type { DB } from "./db/schema.ts";
import {
  createResendEmailClient,
  type EmailClock,
  type FetchLike,
} from "@roost/shared/email-client";
import {
  createEmailOutboxPayloadCipher,
  type EmailOutboxPayload,
  type EmailOutboxPayloadContext,
} from "@roost/shared/email-payload";
import {
  createEmailOutboxDispatcher,
  createKyselyEmailOutboxStore,
  type EmailOutboxDiagnostics,
  type EmailOutboxDispatcherOptions,
  type EmailOutboxRunResult,
} from "./email-outbox.ts";

export interface EmailDeliveryServiceOptions {
  db: Kysely<DB>;
  resendEndpoint: string;
  resendApiKey: string;
  emailFrom: string;
  emailOutboxKey: string;
  fetch?: FetchLike;
  clock?: EmailClock;
  diagnostics?: EmailOutboxDiagnostics;
  dispatcher?: Omit<
    EmailOutboxDispatcherOptions,
    "store" | "client" | "cipher" | "clock" | "diagnostics"
  >;
}

export interface EmailDeliveryService {
  /** Encrypts a rendered message before its enclosing DB transaction inserts it. */
  encryptPayload(context: EmailOutboxPayloadContext, payload: EmailOutboxPayload): string;
  /** Start only from the coordinator lifecycle, after database migrations. */
  start(): void;
  /** Stop from the coordinator shutdown lifecycle. */
  stop(): Promise<void>;
  /** Worker-only hook for focused tests and explicit coordinator wakeups. */
  dispatchOnce(): Promise<EmailOutboxRunResult>;
}

export function createEmailDeliveryService(options: EmailDeliveryServiceOptions): EmailDeliveryService {
  const cipher = createEmailOutboxPayloadCipher(options.emailOutboxKey);
  const client = createResendEmailClient({
    endpoint: options.resendEndpoint,
    apiKey: options.resendApiKey,
    from: options.emailFrom,
    ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
    ...(options.clock === undefined ? {} : { clock: options.clock }),
  });
  const dispatcher = createEmailOutboxDispatcher({
    store: createKyselyEmailOutboxStore(options.db),
    client,
    cipher,
    ...(options.clock === undefined ? {} : { clock: options.clock }),
    ...(options.diagnostics === undefined ? {} : { diagnostics: options.diagnostics }),
    ...options.dispatcher,
  });

  return {
    encryptPayload: (context, payload) => cipher.encrypt(context, payload),
    start: () => dispatcher.start(),
    stop: () => dispatcher.stop(),
    dispatchOnce: () => dispatcher.runOnce(),
  };
}
