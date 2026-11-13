import type { BuiltinCase, Checkpoint, Comparison } from "./scenario";
import { commandCase, effect, literalPattern, observed, send } from "./builtin-steps";

const statsRole = '[data-omp-role="omp.stats"]';
const running = "Dashboard running";
const ignored = "Dashboard already running (requested 127.0.0.1:3848 ignored)";
const hostedSuffix = "; opened in your browser (sessions sync in the background)";
const origin = "http://127.0.0.1:3847";

function stats(pointer: string, expected: unknown): Comparison {
  return effect("builtin.stats", expected, pointer);
}

function realHttpSession(): Comparison[] {
  return [
    stats("/http/dashboardStatus", 200), stats("/http/modelsStatus", 200),
    stats("/http/securityVersion", "3"), stats("/http/bindHostname", "127.0.0.1"),
    stats("/http/contentPreserved", true), stats("/http/sseNativeBytes", true),
    stats("/server/registeredResourceOwned", true),
    stats("/http/syncPhase", "idle"), stats("/http/syncError", null),
    stats("/http/sessionListStatus", 200), stats("/http/sessionListMatchesOriginal", true),
    stats("/http/importedOriginalSession", true), stats("/http/traceStatus", 200),
    stats("/http/traceFileMatchesOriginal", true), stats("/http/traceTitle", "Parity stats workflow"),
    stats("/http/seedUserText", "PARITY_STATS_USER"), stats("/http/seedAssistantText", "PARITY_STATS_ASSISTANT"),
    stats("/http/traceRevalidationStatus", 304), stats("/server/listenerCount", 1),
    effect("builtin.session", true, "/sameSessionId"), effect("builtin.provider", 0, "/requestCount"),
  ];
}

function statsNotice(status: string, hostedTerminal = false): Comparison {
  return {
    name: "Canonical StatsNotice exact launch message",
    terminal: {
      type: "screen", pattern: `(${literalPattern(status)})${literalPattern(hostedTerminal ? hostedSuffix : ` at: ${origin}`)}`, group: 1,
    },
    browser: {
      type: "dom", selector: `${statsRole}:text-is(${JSON.stringify(status + hostedSuffix)}) >> nth=-1`, read: "text",
      pattern: `^(${literalPattern(status)})${literalPattern(hostedSuffix)}$`, group: 1,
    },
    expected: status,
  };
}

function dashboard(status: string, requests: number, accepted: number, hostedTerminal = false): Checkpoint {
  return {
    name: status,
    comparisons: [
      statsNotice(status, hostedTerminal), ...realHttpSession(),
      stats("/host/statsRequestCount", requests), stats("/host/statsAcceptedCount", accepted),
      stats("/host/pendingStatsRequests", 0), stats("/server/sameListenerAsFirst", true),
    ],
  };
}

function launch(command: string, status: string, requests: number, accepted: number, hostedTerminal = false): Checkpoint {
  return { ...send(command, status), ...dashboard(status, requests, accepted, hostedTerminal) };
}

function trace(openCount: number): Checkpoint {
  return {
    name: `Saved-session trace open ${openCount}`,
    comparisons: [
      {
        name: "Actual trace URL status references a nonempty encoded saved session",
        terminal: { type: "screen", pattern: `(Trace:) ${literalPattern(origin)}/#/traces\\?s=([^\\s]+)`, group: 1 },
        browser: { type: "dom", selector: 'text=/Trace: http:\\/\\/127\\.0\\.0\\.1:3847\\/#\\/traces\\?s=/ >> nth=-1', read: "text", pattern: `(Trace:) ${literalPattern(origin)}/#/traces\\?s=([^\\s]+)`, group: 1 },
        expected: "Trace:",
      },
      ...realHttpSession(), stats("/host/traceOpenMatchesOriginal", true),
      stats("/host/traceOpenCount", openCount), stats("/host/traceBlockedNoticeCount", 0),
      stats("/server/sameListenerAsFirst", true),
    ],
  };
}

function openTrace(openCount: number): Checkpoint {
  return { ...send("/trace", "Trace:"), ...trace(openCount) };
}

function restartCleanup(): Checkpoint {
  return send("/restart", "PARITY_STATS_ASSISTANT", [
    effect("builtin.process", 1, "/restartCount"), effect("builtin.session", true, "/sameSessionId"),
    effect("builtin.session", "Parity stats workflow", "/persistedTitle"),
    stats("/cleanup/previousListenerClosed", true), stats("/cleanup/sseClosed", true),
    stats("/cleanup/capabilityUnavailable", true),
    stats("/server/listenerCount", 0),
  ]);
}

function workflow(id: string, command: string, opening: Checkpoint, steps: Checkpoint[], behaviors: string[]): BuiltinCase {
  const scenario = commandCase(id, command, opening.name, steps);
  scenario.checkpoints[0] = { ...opening, name: `1. ${opening.name}` };
  scenario.behaviors = behaviors;
  for (let index = 0; index < scenario.checkpoints.length; index++) {
    const checkpoint = scenario.checkpoints[index];
    const token = index === 0 ? command.split(/\s/, 1)[0].slice(1)
      : checkpoint.terminalInput?.match(/^\/([^\s]+)/)?.[1];
    if (!token) continue;
    for (const comparison of checkpoint.comparisons) {
      if (comparison.expected === undefined || (comparison.expected !== null && typeof comparison.expected === "object")) continue;
      comparison.proves = [{
        entry: `command:${token}`, effect: comparison.terminal.type === "effect" ? "host" : "literal", value: comparison.expected,
        assertionGroups: [{ comparisons: checkpoint.comparisons.map(observation => ({ checkpoint: checkpoint.name, comparison: observation.name })) }],
      }];
    }
  }
  return scenario;
}

function hostFailure(kind: "rejection" | "dismiss"): BuiltinCase {
  const dismissed = kind === "dismiss";
  const message = `Stats dashboard failed: The browser could not open the stats dashboard: ${dismissed ? "The operator dismissed this request." : "The browser did not accept this host service."}`;
  return workflow(`stats-host-${dismissed ? "dismiss" : "rejection"}-recover`, "/stats", observed(message, [
    ...realHttpSession(), stats("/host/statsRequestCount", 1), stats("/host/statsAcceptedCount", 0),
    stats("/host/statsRejectedCount", dismissed ? 0 : 1), stats("/host/statsDismissedCount", dismissed ? 1 : 0),
    stats("/host/pendingStatsRequests", 0), stats("/server/sameListenerAsFirst", true),
  ]), [
    launch("/stats --port=3847 --host=127.0.0.1", running, 2, 1, true),
    launch("/stats --port 3848 --host 127.0.0.1", ignored, 3, 2, true),
    restartCleanup(),
  ], ["stats-real-server-survives-host-refusal", "stats-owned-host-request-settlement", dismissed ? "stats-host-dismissal" : "stats-host-error", "stats-rerun-reuses-server", "stats-owned-process-cleanup"]);
}

export const statsWorkflowCases: BuiltinCase[] = [
  workflow("stats-launch-reuse-trace", "/stats", dashboard(running, 1, 1), [
    launch("/stats -p 3847 --host=127.0.0.1", running, 2, 2),
    launch("/stats --port=3848 --host 127.0.0.1", ignored, 3, 3),
    openTrace(1), openTrace(2), restartCleanup(),
  ], ["stats-canonical-session-judge-launch", "stats-real-http-ingest", "stats-rerun-reuses-server", "stats-requested-address-ignored", "trace-owned-session-open", "trace-rerun-reuses-server", "stats-owned-process-cleanup"]),
  workflow("trace-persisted-rerun-stats", "/trace", trace(1), [
    openTrace(2), launch("/stats --port 3847 --host 127.0.0.1", running, 1, 1),
    launch("/stats --port=3848 --host=127.0.0.1", ignored, 2, 2), restartCleanup(),
  ], ["trace-persisted-first-launch", "trace-owned-session-open", "trace-rerun-reuses-server", "stats-reuses-trace-server", "stats-real-http-ingest", "stats-owned-process-cleanup"]),
  hostFailure("rejection"), hostFailure("dismiss"),
];
