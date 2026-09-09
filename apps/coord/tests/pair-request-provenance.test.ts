import { describe, expect, test } from "bun:test";
import {
  capturePairRequestProvenance,
  describeUserAgent,
  MAX_GEO_UTF8_BYTES,
  MAX_PROVENANCE_UTF8_BYTES,
} from "../src/connect/pair-request-provenance.ts";
import type { CallerOrigin } from "../src/middleware/caller-origin.ts";

const trustedProxy: CallerOrigin = {
  listener: "trusted-proxy",
  clientIp: "203.0.113.7",
  onHost: false,
};
const directPeer: CallerOrigin = {
  listener: "direct",
  clientIp: "198.51.100.9",
  onHost: false,
};

function bytes(value: string | null): number {
  return value === null ? 0 : new TextEncoder().encode(value).byteLength;
}

function headerDouble(values: Record<string, string>): Headers {
  return {
    get(name: string): string | null {
      return values[name.toLowerCase()] ?? null;
    },
  } as unknown as Headers;
}

describe("pair-request provenance", () => {
  test("trusts geo headers and prefers Chromium client hints behind the proxy", () => {
    const captured = capturePairRequestProvenance(new Headers({
      "user-agent": "Mozilla/5.0 Chrome/124.0.0.0 Safari/537.36",
      "sec-ch-ua": '"Not_A Brand";v="99", "Google Chrome";v="124"',
      "sec-ch-ua-platform": '"macOS"',
      "sec-ch-ua-mobile": "?0",
      "cf-ipcountry": "de",
      "cf-region": "Berlin",
      "cf-ipcity": "Berlin",
      "x-forwarded-for": "198.51.100.44",
    }), trustedProxy);

    expect(captured).toEqual({
      userAgent: "Mozilla/5.0 Chrome/124.0.0.0 Safari/537.36",
      clientBrowser: "Chrome",
      clientOs: "macOS",
      clientDeviceType: "desktop",
      sourceIp: "203.0.113.7",
      countryCode: "DE",
      region: "Berlin",
      city: "Berlin",
    });
  });

  test("direct listeners ignore spoofed proxy and visitor-location headers", () => {
    const captured = capturePairRequestProvenance(new Headers({
      "user-agent": "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 Version/17.0 Mobile/15E148 Safari/604.1",
      "x-forwarded-for": "203.0.113.7",
      "cf-ipcountry": "DE",
      "cf-region": "Berlin",
      "cf-ipcity": "Berlin",
    }), directPeer);

    expect(captured.sourceIp).toBe("198.51.100.9");
    expect(captured.countryCode).toBeNull();
    expect(captured.region).toBeNull();
    expect(captured.city).toBeNull();
    expect(captured.clientBrowser).toBe("Safari");
    expect(captured.clientOs).toBe("iOS");
    expect(captured.clientDeviceType).toBe("mobile");
  });

  test("removes controls and bounds every persisted header value", () => {
    const captured = capturePairRequestProvenance(headerDouble({
      "user-agent": `Chrome/124\r\nInjected\u0000${"x".repeat(10_000)}`,
      "cf-ipcountry": "uS",
      "cf-region": `region-${"r".repeat(200)}`,
      "cf-ipcity": `city-${"c".repeat(200)}`,
    }), trustedProxy);

    expect(bytes(captured.userAgent)).toBeLessThanOrEqual(MAX_PROVENANCE_UTF8_BYTES);
    expect(bytes(captured.region)).toBeLessThanOrEqual(MAX_GEO_UTF8_BYTES);
    expect(bytes(captured.city)).toBeLessThanOrEqual(MAX_GEO_UTF8_BYTES);
    expect(captured.userAgent).not.toMatch(/[\u0000-\u001f\u007f-\u009f]/);
    expect(captured.userAgent).toMatch(/^Chrome\/124Injected/);
    expect(captured.countryCode).toBe("US");
  });

  test("fallback parser keeps precedence for Edge and tablet user agents", () => {
    expect(describeUserAgent(
      "Mozilla/5.0 (iPad; CPU OS 17_0) AppleWebKit/605.1.15 "
      + "CriOS/124.0.0.0 Mobile/15E148 Safari/604.1",
    )).toEqual({ browser: "Safari", os: "iOS", deviceType: "tablet" });

    expect(describeUserAgent(
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      + "Chrome/124.0.0.0 Safari/537.36 Edg/124.0.0.0",
    )).toEqual({ browser: "Edge", os: "Windows", deviceType: "desktop" });
  });
});
