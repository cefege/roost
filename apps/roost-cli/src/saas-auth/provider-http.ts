/**
 * Fetches bounded JSON from allowlisted external identity and abuse providers.
 * Google and Turnstile protocols inject this boundary for deterministic network handling.
 * Timeout and response limits keep provider failures from expanding the gateway attack surface.
 */

import { parseStrictProviderJson } from "./canonical-json.ts";

export type GatewayFetch = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export class ProviderRequestError extends Error {
  constructor(readonly code: "timeout" | "network" | "response") {
    super("provider request failed");
    this.name = "ProviderRequestError";
  }
}

const timeoutMarker = Symbol("provider-timeout");

async function readProviderBody(response: Response, maxBytes: number, maxChunks: number): Promise<Uint8Array> {
  const declared = response.headers.get("content-length");
  let expected: number | null = null;
  if (declared !== null) {
    if (!/^(0|[1-9][0-9]*)$/u.test(declared)) throw new ProviderRequestError("response");
    expected = Number(declared);
    if (!Number.isSafeInteger(expected) || expected <= 0 || expected > maxBytes) {
      throw new ProviderRequestError("response");
    }
  }
  if (response.body === null) throw new ProviderRequestError("response");
  const target = new Uint8Array(maxBytes);
  const reader = response.body.getReader();
  let chunks = 0;
  let length = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks++;
      if (chunks > maxChunks || length + value.byteLength > maxBytes) {
        throw new ProviderRequestError("response");
      }
      target.set(value, length);
      length += value.byteLength;
    }
  } finally {
    reader.releaseLock();
  }
  if (length === 0 || (expected !== null && expected !== length)) throw new ProviderRequestError("response");
  return target.slice(0, length);
}

export async function fetchBoundedJson(
  fetchImpl: GatewayFetch,
  input: string,
  init: RequestInit,
  options: { timeoutMs: number; maxResponseBytes: number; maxResponseChunks?: number },
): Promise<{ response: Response; value: unknown }> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), options.timeoutMs);
  let response: Response;
  try {
    response = await Promise.race([
      fetchImpl(input, { ...init, redirect: "manual", signal: controller.signal }),
      new Promise<never>((_, reject) => {
        const timeout = setTimeout(() => reject(timeoutMarker), options.timeoutMs);
        timeout.unref?.();
      }),
    ]);
  } catch (error) {
    if (error === timeoutMarker || controller.signal.aborted) throw new ProviderRequestError("timeout");
    throw new ProviderRequestError("network");
  } finally {
    clearTimeout(timer);
  }
  let value: unknown;
  try {
    const bytes = await readProviderBody(
      response,
      options.maxResponseBytes,
      options.maxResponseChunks ?? 32,
    );
    value = parseStrictProviderJson(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch (error) {
    if (error instanceof ProviderRequestError) throw error;
    throw new ProviderRequestError("response");
  }
  return { response, value };
}
