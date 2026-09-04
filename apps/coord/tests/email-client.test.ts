import { describe, expect, test } from "bun:test";
import {
  createResendEmailClient,
  type EmailClock,
  type FetchLike,
  type EmailTimer,
  type ResendEmailMessage,
} from "@roost/shared/email-client";

class FakeClock implements EmailClock {
  nowMs = 1_000_000;
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
    if (!entry) throw new Error("no pending timer");
    this.timers.delete(entry[0]);
    entry[1]();
  }

  pendingTimerCount(): number {
    return this.timers.size;
  }
}

const message: ResendEmailMessage = {
  recipient: "recipient@example.test",
  subject: "Private invitation subject",
  html: "<p>secret invitation body</p>",
  text: "secret invitation body",
  idempotencyKey: "outbox-row-123",
};

describe("Resend email client", () => {
  test("sends the documented request and durable idempotency header", async () => {
    let requestUrl = "";
    let requestHeaders = new Headers();
    let requestBody = "";

    const fakeFetch: FetchLike = async (input, init) => {
      requestUrl = String(input);
      requestHeaders = new Headers(init?.headers);
      requestBody = String(init?.body);
      return new Response(JSON.stringify({ id: "provider-message-1" }), { status: 200 });
    };
    const client = createResendEmailClient({
      endpoint: "https://api.resend.test/emails",
      apiKey: "resend-api-secret",
      from: "Roost <no-reply@example.test>",
      fetch: fakeFetch,
    });

    await expect(client.send(message)).resolves.toEqual({
      outcome: "sent",
      providerMessageId: "provider-message-1",
    });
    expect(requestUrl).toBe("https://api.resend.test/emails");
    expect(requestHeaders.get("authorization")).toBe("Bearer resend-api-secret");
    expect(requestHeaders.get("content-type")).toBe("application/json");
    expect(requestHeaders.get("idempotency-key")).toBe("outbox-row-123");
    expect(JSON.parse(requestBody)).toEqual({
      from: "Roost <no-reply@example.test>",
      to: ["recipient@example.test"],
      subject: "Private invitation subject",
      html: "<p>secret invitation body</p>",
      text: "secret invitation body",
    });
  });

  test("aborts a hung injected fetch at the injected timeout", async () => {
    const clock = new FakeClock();
    let aborted = false;

    const fakeFetch: FetchLike = async (_input, init) => new Promise<Response>((_resolve, reject) => {
      init?.signal?.addEventListener("abort", () => {
        aborted = true;
        reject(new DOMException("aborted", "AbortError"));
      }, { once: true });
    });

    const client = createResendEmailClient({
      endpoint: "https://api.resend.test/emails",
      apiKey: "resend-api-secret",
      from: "Roost <no-reply@example.test>",
      clock,
      timeoutMs: 100,
      fetch: fakeFetch,
    });

    const send = client.send(message);
    expect(clock.pendingTimerCount()).toBe(1);
    clock.fireNext();
    await expect(send).resolves.toEqual({ outcome: "retry", reason: "timeout" });
    expect(aborted).toBe(true);
    expect(clock.pendingTimerCount()).toBe(0);
  });

  test("classifies 408, 429, and 5xx as retryable and other 4xx as terminal", async () => {
    const clock = new FakeClock();
    const statuses = [
      { status: 408, retry: true, retryAfterMs: undefined },
      { status: 429, retry: true, retryAfterMs: 7_000 },
      { status: 503, retry: true, retryAfterMs: undefined },
      { status: 400, retry: false, retryAfterMs: undefined },
    ];

    for (const expected of statuses) {
      const fakeFetch: FetchLike = async () => new Response(null, {
        status: expected.status,
        headers: expected.status === 429 ? { "Retry-After": "7" } : undefined,
      });
      const client = createResendEmailClient({
        endpoint: "https://api.resend.test/emails",
        apiKey: "resend-api-secret",
        from: "Roost <no-reply@example.test>",
        clock,
        fetch: fakeFetch,
      });
      const result = await client.send(message);
      if (expected.retry) {
        expect(result).toEqual({
          outcome: "retry",
          reason: `http_${expected.status}`,
          ...(expected.retryAfterMs === undefined ? {} : { retryAfterMs: expected.retryAfterMs }),
        });
      } else {
        expect(result).toEqual({ outcome: "permanent", reason: `http_${expected.status}` });
      }
    }

    const fakeFetch: FetchLike = async () => {
      throw new Error("network unavailable");
    };

    const networkClient = createResendEmailClient({
      endpoint: "https://api.resend.test/emails",
      apiKey: "resend-api-secret",
      from: "Roost <no-reply@example.test>",
      fetch: fakeFetch,
    });
    await expect(networkClient.send(message)).resolves.toEqual({ outcome: "retry", reason: "network" });
  });
});
