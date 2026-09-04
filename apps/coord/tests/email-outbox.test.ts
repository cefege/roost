import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { DbHandle, KyselyDB } from "../src/db/connection.ts";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import type {
  EmailClient,
  EmailClock,
  FetchLike,
  EmailTimer,
  ResendEmailMessage,
  ResendEmailResult,
} from "@roost/shared/email-client";
import { createEmailDeliveryService } from "../src/email-delivery.ts";
import {
  createEmailOutboxDispatcher,
  createKyselyEmailOutboxStore,
  type EmailOutboxDiagnosticEvent,
  type EmailOutboxDiagnosticFields,
  type EmailOutboxDiagnostics,
} from "../src/email-outbox.ts";
import { createEmailOutboxPayloadCipher } from "@roost/shared/email-payload";

const NOW_MS = 1_000_000;
const OUTBOX_KEY = Buffer.alloc(32, 7).toString("base64");
const RECIPIENT = "recipient@example.test";
const SUBJECT = "Private invitation subject";
const TOKEN = "invite-token-that-must-not-log";
const BODY = `<p>Use ${TOKEN}</p>`;

class FixedClock implements EmailClock {
  nowMs = NOW_MS;
  private nextTimer = 1;
  private timers = new Map<number, () => void>();

  now(): number {
    return this.nowMs;
  }

  setTimeout(callback: () => void, _delayMs: number): EmailTimer {
    const timer = this.nextTimer++;
    this.timers.set(timer, callback);
    return timer as unknown as EmailTimer;
  }

  clearTimeout(timer: EmailTimer): void {
    const timerId = timer as unknown as number;
    this.timers.delete(timerId);
  }

  fireNext(): void {
    const [entry] = this.timers.entries();
    if (!entry) throw new Error("no scheduled email dispatcher timer");
    this.timers.delete(entry[0]);
    entry[1]();
  }
}
interface CapturedDiagnostic {
  level: "info" | "warn";
  event: EmailOutboxDiagnosticEvent;
  fields: EmailOutboxDiagnosticFields;
}

function diagnosticsInto(entries: CapturedDiagnostic[]): EmailOutboxDiagnostics {
  return {
    info(event, fields): void {
      entries.push({ level: "info", event, fields });
    },
    warn(event, fields): void {
      entries.push({ level: "warn", event, fields });
    },
  };
}

function resultClient(
  resultForRecipient: (recipient: string) => ResendEmailResult,
  requests: ResendEmailMessage[],
): EmailClient {
  return {
    async send(message): Promise<ResendEmailResult> {
      requests.push(message);
      return resultForRecipient(message.recipient);
    },
  };
}

let workdir: string;
let opened: DbHandle;
let db: KyselyDB;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-email-outbox-"));
  opened = openDb(join(workdir, "coord.db"));
  db = opened.db;
  await runMigrations(opened.sqlite);
});

beforeEach(async () => {
  await db.deleteFrom("email_outbox").execute();
});

afterAll(async () => {
  try {
    await opened.close();
  } finally {
    if (workdir) rmSync(workdir, { recursive: true, force: true });
  }
});

async function insertOutboxRow(options: {
  id: string;
  recipient?: string;
  idempotencyKey?: string;
  state?: "pending" | "sending";
  attempts?: number;
  lockedUntilMs?: number | null;
  leaseToken?: string | null;
  nextAttemptMs?: number;
  encryptedPayload?: string;
}): Promise<void> {
  const cipher = createEmailOutboxPayloadCipher(OUTBOX_KEY);
  const recipient = options.recipient ?? RECIPIENT;
  const encryptedPayload = options.encryptedPayload ?? cipher.encrypt(
    { outboxId: options.id, kind: "invitation" },
    { subject: SUBJECT, html: BODY, text: TOKEN },
  );
  await db.insertInto("email_outbox").values({
    id: options.id,
    kind: "invitation",
    recipient,
    encrypted_payload: encryptedPayload,
    idempotency_key: options.idempotencyKey ?? options.id,
    state: options.state ?? "pending",
    attempts: options.attempts ?? 0,
    locked_until_ms: options.lockedUntilMs ?? null,
    lease_token: options.leaseToken ?? null,
    next_attempt_ms: options.nextAttemptMs ?? NOW_MS,
    provider_message_id: null,
    sent_at_ms: null,
    failed_at_ms: null,
    last_error: null,
  }).execute();
}

function row(id: string) {
  return db.selectFrom("email_outbox").selectAll().where("id", "=", id).executeTakeFirstOrThrow();
}

describe("durable email outbox", () => {
  test("atomically leases once, sends with persisted row ID, and emits redacted diagnostics", async () => {
    const id = "outbox-success";
    await insertOutboxRow({ id, idempotencyKey: "legacy-incorrect-key" });
    const encryptedBeforeDelivery = (await row(id)).encrypted_payload;
    expect(encryptedBeforeDelivery).not.toContain(TOKEN);
    expect(encryptedBeforeDelivery).not.toContain(BODY);
    const requests: ResendEmailMessage[] = [];
    const diagnostics: CapturedDiagnostic[] = [];
    const shared = {
      store: createKyselyEmailOutboxStore(db),
      cipher: createEmailOutboxPayloadCipher(OUTBOX_KEY),
      clock: new FixedClock(),
      diagnostics: diagnosticsInto(diagnostics),
      client: resultClient(() => ({ outcome: "sent", providerMessageId: "provider-message-1" }), requests),
    };
    const first = createEmailOutboxDispatcher(shared);
    const second = createEmailOutboxDispatcher(shared);

    const [one, two] = await Promise.all([first.runOnce(), second.runOnce()]);
    expect(one.sent + two.sent).toBe(1);
    expect(requests).toHaveLength(1);
    expect(requests[0]?.idempotencyKey).toBe(id);
    expect(requests[0]?.recipient).toBe(RECIPIENT);
    expect(requests[0]?.html).toBe(BODY);

    const stored = await row(id);
    expect(stored).toMatchObject({
      state: "sent",
      attempts: 1,
      locked_until_ms: null,
      lease_token: null,
      provider_message_id: "provider-message-1",
      sent_at_ms: NOW_MS,
      last_error: null,
    });

    const diagnosticText = JSON.stringify(diagnostics);
    for (const sensitive of [RECIPIENT, SUBJECT, TOKEN, BODY, OUTBOX_KEY, "legacy-incorrect-key"]) {
      expect(diagnosticText).not.toContain(sensitive);
    }
    expect(diagnostics).toEqual([{
      level: "info",
      event: "email_sent",
      fields: { outbox_id: id, kind: "invitation", attempt: 1 },
    }]);
  });

  test("factory composes encrypted payload, Resend client, and dispatcher without exposing credentials", async () => {
    const id = "outbox-factory";
    const clock = new FixedClock();
    const diagnostics: CapturedDiagnostic[] = [];
    let idempotencyKey = "";

    const fakeFetch: FetchLike = async (_input, init) => {
      idempotencyKey = new Headers(init?.headers).get("idempotency-key") ?? "";
      return new Response(JSON.stringify({ id: "factory-provider-id" }), { status: 200 });
    };

    const service = createEmailDeliveryService({
      db,
      resendEndpoint: "https://api.resend.test/emails",
      resendApiKey: "factory-api-secret",
      emailFrom: "Roost <no-reply@example.test>",
      emailOutboxKey: OUTBOX_KEY,
      clock,
      diagnostics: diagnosticsInto(diagnostics),
      dispatcher: { batchSize: 1 },
      fetch: fakeFetch,
    });
    const encryptedPayload = service.encryptPayload(
      { outboxId: id, kind: "invitation" },
      { subject: SUBJECT, html: BODY, text: TOKEN },
    );
    expect(encryptedPayload).not.toContain(TOKEN);
    await insertOutboxRow({ id, encryptedPayload });

    await expect(service.dispatchOnce()).resolves.toEqual({ claimed: 1, sent: 1, retried: 0, failed: 0 });
    expect(idempotencyKey).toBe(id);
    expect(JSON.stringify(diagnostics)).not.toContain("factory-api-secret");
    await service.stop();
  });

  test("lifecycle starts timer-owned delivery and stops after the active pass", async () => {
    const id = "outbox-lifecycle";
    const clock = new FixedClock();
    let signalDelivery!: () => void;
    const deliveryStarted = new Promise<void>((resolve) => {
      signalDelivery = resolve;
    });

    const fakeFetch: FetchLike = async () => {
      signalDelivery();
      return new Response(JSON.stringify({ id: "lifecycle-provider-id" }), { status: 200 });
    };

    const service = createEmailDeliveryService({
      db,
      resendEndpoint: "https://api.resend.test/emails",
      resendApiKey: "lifecycle-api-secret",
      emailFrom: "Roost <no-reply@example.test>",
      emailOutboxKey: OUTBOX_KEY,
      clock,
      dispatcher: { batchSize: 1 },
      fetch: fakeFetch,
    });
    await insertOutboxRow({
      id,
      encryptedPayload: service.encryptPayload(
        { outboxId: id, kind: "invitation" },
        { subject: SUBJECT, html: BODY, text: TOKEN },
      ),
    });

    service.start();
    clock.fireNext();
    await deliveryStarted;
    await service.stop();
    await expect(row(id)).resolves.toMatchObject({
      state: "sent",
      provider_message_id: "lifecycle-provider-id",
    });
  });

  test("retries timeout, 429, and 5xx with bounded Retry-After plus backoffDelayMs", async () => {
    const ids = {
      timeout: "outbox-timeout",
      rateLimited: "outbox-rate-limited",
      serverError: "outbox-server-error",
    };
    await Promise.all([
      insertOutboxRow({ id: ids.timeout, recipient: "timeout@example.test" }),
      insertOutboxRow({ id: ids.rateLimited, recipient: "rate@example.test" }),
      insertOutboxRow({ id: ids.serverError, recipient: "server@example.test" }),
    ]);
    const requests: ResendEmailMessage[] = [];
    const dispatcher = createEmailOutboxDispatcher({
      store: createKyselyEmailOutboxStore(db),
      cipher: createEmailOutboxPayloadCipher(OUTBOX_KEY),
      clock: new FixedClock(),
      client: resultClient((recipient) => {
        if (recipient === "timeout@example.test") return { outcome: "retry", reason: "timeout" };
        if (recipient === "rate@example.test") {
          return { outcome: "retry", reason: "http_429", retryAfterMs: 86_400_000 };
        }
        return { outcome: "retry", reason: "http_503" };
      }, requests),
      batchSize: 3,
      maxRetryAfterMs: 5_000,
      backoff: { baseMs: 100, maxMs: 1_000, jitter: "none" },
    });

    await expect(dispatcher.runOnce()).resolves.toEqual({ claimed: 3, sent: 0, retried: 3, failed: 0 });
    expect(requests).toHaveLength(3);
    await expect(row(ids.timeout)).resolves.toMatchObject({
      state: "pending", attempts: 1, next_attempt_ms: NOW_MS + 100, last_error: "timeout",
    });
    await expect(row(ids.rateLimited)).resolves.toMatchObject({
      state: "pending", attempts: 1, next_attempt_ms: NOW_MS + 5_000, last_error: "http_429",
    });
    await expect(row(ids.serverError)).resolves.toMatchObject({
      state: "pending", attempts: 1, next_attempt_ms: NOW_MS + 100, last_error: "http_503",
    });
  });

  test("marks other 4xx and exhausted retry budget terminally", async () => {
    await Promise.all([
      insertOutboxRow({ id: "outbox-permanent", recipient: "permanent@example.test" }),
      insertOutboxRow({ id: "outbox-exhausted", recipient: "exhausted@example.test", attempts: 2 }),
    ]);
    const dispatcher = createEmailOutboxDispatcher({
      store: createKyselyEmailOutboxStore(db),
      cipher: createEmailOutboxPayloadCipher(OUTBOX_KEY),
      clock: new FixedClock(),
      client: resultClient((recipient) => recipient === "permanent@example.test"
        ? { outcome: "permanent", reason: "http_400" }
        : { outcome: "retry", reason: "http_500" }, []),
      batchSize: 2,
      maxAttempts: 3,
      backoff: { baseMs: 100, maxMs: 1_000, jitter: "none" },
    });

    await expect(dispatcher.runOnce()).resolves.toEqual({ claimed: 2, sent: 0, retried: 0, failed: 2 });
    await expect(row("outbox-permanent")).resolves.toMatchObject({
      state: "failed", attempts: 1, failed_at_ms: NOW_MS, last_error: "http_400",
    });
    await expect(row("outbox-exhausted")).resolves.toMatchObject({
      state: "failed", attempts: 3, failed_at_ms: NOW_MS, last_error: "retry_exhausted",
    });
  });

  test("does not send a recovered lease that already exceeds its retry budget", async () => {
    const id = "outbox-expired-exhausted";
    await insertOutboxRow({
      id,
      state: "sending",
      attempts: 3,
      lockedUntilMs: NOW_MS - 1,
      leaseToken: "abandoned-lease",
    });
    const requests: ResendEmailMessage[] = [];
    const dispatcher = createEmailOutboxDispatcher({
      store: createKyselyEmailOutboxStore(db),
      cipher: createEmailOutboxPayloadCipher(OUTBOX_KEY),
      clock: new FixedClock(),
      client: resultClient(() => ({ outcome: "sent", providerMessageId: "must-not-send" }), requests),
      maxAttempts: 3,
    });

    await expect(dispatcher.runOnce()).resolves.toEqual({ claimed: 1, sent: 0, retried: 0, failed: 1 });
    expect(requests).toHaveLength(0);
    await expect(row(id)).resolves.toMatchObject({
      state: "failed",
      attempts: 4,
      last_error: "retry_exhausted",
    });
  });

  test("recovers expired leases and rejects a stale lease owner's completion", async () => {
    const id = "outbox-expired-lease";
    await insertOutboxRow({
      id,
      state: "sending",
      attempts: 1,
      lockedUntilMs: NOW_MS - 1,
      leaseToken: "abandoned-lease",
    });
    const store = createKyselyEmailOutboxStore(db);
    const [recovered] = await store.claimDue({ nowMs: NOW_MS, leaseDurationMs: 60_000, limit: 1 });
    if (!recovered) throw new Error("expected expired lease recovery");

    const staleApplied = await store.markSent({ ...recovered, leaseToken: "abandoned-lease" }, "stale-provider-id", NOW_MS);
    expect(staleApplied).toBe(false);
    expect(await store.markSent(recovered, "recovered-provider-id", NOW_MS)).toBe(true);
    await expect(row(id)).resolves.toMatchObject({
      state: "sent",
      attempts: 2,
      provider_message_id: "recovered-provider-id",
      lease_token: null,
      locked_until_ms: null,
    });
  });
});
