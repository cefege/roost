// Shared `roost status` test scaffolding: a healthy report to override, the
// stdout capture the renderer's line assertions need, and the fetch stub shape
// the probe helpers accept. Used by status.test.ts and status-spa.test.ts so
// one report shape change does not have to be mirrored per file.

import { printStatusReport, type StatusReport } from "../src/status.ts";

export function statusReportFixture(overrides: Partial<StatusReport> = {}): StatusReport {
  return {
    coordAgentLoaded: true,
    workerAgentLoaded: true,
    coord: { reachable: true, gitSha: null },
    workers: [],
    endpoint: { publicUrl: "https://dash.example.test", answers: true },
    spa: { serves: true, webDistPath: "/repo/apps/web/dist", webDistPresent: true },
    ...overrides,
  };
}

export function renderedStatusLines(report: StatusReport): string[] {
  const lines: string[] = [];
  const originalLog = console.log;
  console.log = (...args: unknown[]) => { lines.push(args.join(" ")); };
  try {
    printStatusReport(report);
  } finally {
    console.log = originalLog;
  }
  return lines;
}

type TestFetchImplementation = (
  input: string | URL | Request,
  init?: BunFetchRequestInit,
) => Promise<Response>;

export function testFetch(implementation: TestFetchImplementation): typeof fetch {
  return Object.assign(implementation, { preconnect: fetch.preconnect });
}
