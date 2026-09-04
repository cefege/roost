// Provider-neutral Resend HTTPS client. It deliberately returns only
// classified, non-sensitive outcomes: callers must never log request data,
// provider response bodies, credentials, recipients, or rendered email bodies.

export type EmailTimer = Timer;

/** The email client only calls fetch; Bun's unrelated `preconnect` static
 * helper is deliberately not part of this injectable seam. */
export type FetchLike = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

export interface EmailClock {
  now(): number;
  setTimeout(callback: () => void, delayMs: number): EmailTimer;
  clearTimeout(timer: EmailTimer): void;
}

const systemClock: EmailClock = {
  now: Date.now,
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (timer) => clearTimeout(timer),
};

export interface ResendEmailMessage {
  recipient: string;
  subject: string;
  html: string;
  text?: string;
  /** Must be the durable outbox row ID, never a newly generated retry ID. */
  idempotencyKey: string;
}

export type ResendFailureReason =
  | "timeout"
  | "network"
  | "invalid_message"
  | "invalid_success_response"
  | `http_${number}`
  | "http_other";

export interface ResendEmailSent {
  outcome: "sent";
  providerMessageId: string;
}

export interface ResendEmailRetry {
  outcome: "retry";
  reason: ResendFailureReason;
  /** Parsed but deliberately unbounded here; the dispatcher applies its cap. */
  retryAfterMs?: number;
}

export interface ResendEmailPermanentFailure {
  outcome: "permanent";
  reason: ResendFailureReason;
}

export type ResendEmailResult = ResendEmailSent | ResendEmailRetry | ResendEmailPermanentFailure;

export interface EmailClient {
  send(message: ResendEmailMessage): Promise<ResendEmailResult>;
}

export interface ResendEmailClientOptions {
  /** Exact HTTPS Resend emails endpoint, normally https://api.resend.com/emails. */
  endpoint: string;
  /** Boot-only credential. This module never returns or logs it. */
  apiKey: string;
  from: string;
  fetch?: FetchLike;
  clock?: EmailClock;
  timeoutMs?: number;
}

const DEFAULT_TIMEOUT_MS = 15_000;
const MAX_IDEMPOTENCY_KEY_LENGTH = 256;
const timeoutSentinel = Symbol("resend_timeout");

function validMessage(message: ResendEmailMessage): boolean {
  return message.recipient.length > 0
    && message.subject.length > 0
    && message.html.length > 0
    && message.idempotencyKey.length > 0
    && message.idempotencyKey.length <= MAX_IDEMPOTENCY_KEY_LENGTH;
}

function httpReason(status: number): ResendFailureReason {
  // Keep the diagnostic vocabulary bounded. A status code contains no customer
  // data, while response bodies routinely do and must not escape this boundary.
  return status >= 300 && status <= 599 ? `http_${status}` : "http_other";
}

function retryAfterMs(value: string | null, nowMs: number): number | undefined {
  if (!value) return undefined;
  const seconds = Number(value);
  if (Number.isFinite(seconds) && seconds >= 0) {
    return Math.floor(seconds * 1_000);
  }
  const atMs = Date.parse(value);
  if (!Number.isFinite(atMs)) return undefined;
  return Math.max(0, atMs - nowMs);
}

function isProviderSuccess(value: unknown): value is { id: string } {
  return typeof value === "object"
    && value !== null
    && "id" in value
    && typeof value.id === "string"
    && value.id.length > 0;
}

/**
 * Creates an injected-fetch Resend client. Every retry of one outbox row must
 * pass that row's ID as `idempotencyKey`; the client forwards it verbatim as
 * Resend's documented Idempotency-Key header.
 */
export function createResendEmailClient(options: ResendEmailClientOptions): EmailClient {
  const fetchImpl = options.fetch ?? globalThis.fetch;
  const clock = options.clock ?? systemClock;
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) throw new Error("invalid Resend timeout");

  return {
    async send(message: ResendEmailMessage): Promise<ResendEmailResult> {
      if (!validMessage(message)) return { outcome: "permanent", reason: "invalid_message" };

      const controller = new AbortController();
      let timer: EmailTimer | undefined;
      let timedOut = false;
      const timeout = new Promise<never>((_, reject) => {
        timer = clock.setTimeout(() => {
          timedOut = true;
          controller.abort();
          reject(timeoutSentinel);
        }, timeoutMs);
      });

      const request = (async (): Promise<ResendEmailResult> => {
        let response: Response;
        try {
          response = await fetchImpl(options.endpoint, {
            method: "POST",
            headers: {
              Authorization: `Bearer ${options.apiKey}`,
              "Content-Type": "application/json",
              "Idempotency-Key": message.idempotencyKey,
            },
            body: JSON.stringify({
              from: options.from,
              to: [message.recipient],
              subject: message.subject,
              html: message.html,
              ...(message.text === undefined ? {} : { text: message.text }),
            }),
            signal: controller.signal,
          });
        } catch {
          return { outcome: "retry", reason: timedOut ? "timeout" : "network" };
        }

        if (response.status === 408 || response.status === 429 || response.status >= 500) {
          return {
            outcome: "retry",
            reason: httpReason(response.status),
            retryAfterMs: retryAfterMs(response.headers.get("retry-after"), clock.now()),
          };
        }
        if (!response.ok) {
          return { outcome: "permanent", reason: httpReason(response.status) };
        }

        try {
          const payload: unknown = await response.json();
          if (!isProviderSuccess(payload)) {
            return { outcome: "retry", reason: "invalid_success_response" };
          }
          return { outcome: "sent", providerMessageId: payload.id };
        } catch {
          // A dropped/corrupt successful response is not a provider success.
          // Retrying is safe because the durable row ID remains idempotent.
          return { outcome: "retry", reason: "invalid_success_response" };
        }
      })();

      try {
        return await Promise.race([request, timeout]);
      } catch (error) {
        if (error === timeoutSentinel) return { outcome: "retry", reason: "timeout" };
        // `request` classifies its own failures, but never surface an unknown
        // Error message because fetch implementations may include request data.
        return { outcome: "retry", reason: "network" };
      } finally {
        if (timer !== undefined) clock.clearTimeout(timer);
      }
    },
  };
}
