import { describe, expect, test } from "bun:test";
import { printStatusReport, resolveTlsMode, type StatusReport } from "../src/status.ts";

describe("status TLS topology", () => {
  test("fronted Linux services require the configured Tailscale Serve mapping", () => {
    const service = [
      "[Service]",
      "Environment=ROOST_FRONTED=1",
      "Environment=ROOST_COORD_LOOPBACK_PORT=4213",
    ].join("\n");
    expect(resolveTlsMode(service, "https://host:4102 -> http://127.0.0.1:4213", "linux"))
      .toBe("tailscale-serve");
    expect(resolveTlsMode(service, "https://host:4102 -> http://127.0.0.1:4103", "linux"))
      .toBe("missing");
  });

  test("fronted Darwin services use the default loopback port when omitted", () => {
    const plist = "<key>ROOST_FRONTED</key>\n<string>1</string>";
    expect(resolveTlsMode(plist, "https://host:4102 -> http://127.0.0.1:4103", "darwin"))
      .toBe("tailscale-serve");
  });

  test("direct mode requires both certificate and key paths", () => {
    const complete = [
      "Environment=ROOST_TLS_CERT_PATH=/data/tls/host.crt",
      "Environment=ROOST_TLS_KEY_PATH=/data/tls/host.key",
    ].join("\n");
    expect(resolveTlsMode(complete, null, "linux")).toBe("direct");
    expect(resolveTlsMode("Environment=ROOST_TLS_CERT_PATH=/data/tls/host.crt", null, "linux"))
      .toBe("missing");
  });

  test("current fronted topology renders TLS healthy", () => {
    const report: StatusReport = {
      tailscale: { state: "Running", fqdn: "host.tail.ts.net", running: true },
      coordAgentLoaded: true,
      workerAgentLoaded: true,
      coord: { reachable: true, gitSha: "abcdef123456" },
      workers: [],
      tlsMode: "tailscale-serve",
      url: "https://host.tail.ts.net:4102",
      handoff: null,
    };
    const lines: string[] = [];
    const originalLog = console.log;
    console.log = (...args: unknown[]) => { lines.push(args.join(" ")); };
    try {
      printStatusReport(report);
    } finally {
      console.log = originalLog;
    }
    expect(lines).toContain("  ✓ coord TLS: tailscale serve");
    expect(lines.join("\n")).not.toContain("mint: tailscale cert");
  });
});
