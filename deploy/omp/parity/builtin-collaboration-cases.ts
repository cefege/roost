import type { BuiltinCase } from "./scenario";
import { commandCase, dismissThenSend, effect, send } from "./builtin-steps";

export const collaborationCases: BuiltinCase[] = [
  commandCase("advisor", "/advisor off", "Advisor disabled.", [send("/advisor on", "Advisor setting enabled, but no model is assigned to the 'advisor' role."), send("/advisor off", "Advisor disabled."), send("/advisor dump", "Advisor is not active for this session."), send("/advisor dump raw", "Advisor is not active for this session."), send("/advisor invalid", "Usage: /advisor [on|off|status|dump [raw]|configure]")]),
  commandCase("advisor-configure-cancel", "/advisor configure", "Shared instructions", [dismissThenSend("/advisor off", "Advisor disabled.")]),
  commandCase("export-copy-error", "/export copy", "Use /dump to copy the session to clipboard."),
  commandCase("export-file", "/export parity-export.html", "Session exported to:", [], [effect("builtin.workspace-file", true, "/parity-export.html/exists"), effect("builtin.workspace-file", true, "/parity-export.html/containsSeedTranscript")]),
  commandCase("trace-empty", "/trace", "No session file yet — send a message first."),
  commandCase("dump-empty", "/dump", "No messages to dump yet.", [send("/dump all", "No messages to dump yet."), send("/dump anon", "No messages to dump yet."), send("/dump invalid", "Usage: /dump [all|anon]")]),
  commandCase("share-error", "/share", "HTTP 503 (parity share boundary refused)", [], [effect("builtin.http", 1, "/share/requests")]),
  commandCase("collab-empty", "/collab status", "Not in a collab session", [send("/collab list", "No active Collab hosts"), send("/collab stop", "Collab stopped"), send("/collab list invalid", "Usage: /collab list — for links or JSON use `omp collab link|list`")]),
  commandCase("collab-refused", "/collab start ws://127.0.0.1:{{relayPort}}", "Failed to start collab session: room is full"),
  commandCase("collab-view-refused", "/collab view ws://127.0.0.1:{{relayPort}}", "Failed to start collab session: room is full"),
  commandCase("join-empty", "/join", "Usage: /join <link>"),
  commandCase("leave-empty", "/leave", "Not in a collab session"),
  commandCase("browser-disabled", "/browser", "Browser capability is disabled (enable in settings)"),
  commandCase("browser-toggle", "/browser headless", "Browser mode: headless", [send("/browser visible", "Browser mode: visible", [effect("builtin.persisted-settings", false, "/browser/headless")]), send("/browser hidden", "Browser mode: headless"), send("/browser show", "Browser mode: visible"), send("/browser headful", "Browser mode: visible"), send("/browser invalid", "Usage: /browser [headless|visible]")]),
  commandCase("copy-empty", "/copy code", "No code block to copy.", [send("/copy cmd", "No command to copy."), send("/copy command", "No command to copy."), send("/copy link", "No link to copy."), send("/copy url", "No link to copy."), send("/copy invalid", "Usage: /copy [code|cmd|link]")]),
  commandCase("copy-populated", "/copy code", "Copied code block to clipboard", [send("/copy link", "Copied link to clipboard", [effect("builtin.clipboard", "https://example.invalid/parity")])], [effect("builtin.clipboard", "const parity = 42;")]),
  commandCase("open-empty", "/open", "No link to open.", [send("/open link", "No link to open."), send("/open url", "No link to open.")]),
  commandCase("open-link", "/open", "Opening https://example.invalid/parity", [], [effect("builtin.host-open-url", "https://example.invalid/parity")]),
];

for (const scenario of collaborationCases) {
  scenario.settings = { "advisor.enabled": false, "browser.enabled": scenario.id === "builtin-browser-toggle", ...scenario.settings };
}
