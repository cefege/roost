import type { BuiltinCase } from "./scenario";
import { commandCase, composer, effect, escape, observed, send } from "./builtin-steps";

export const controlCases: BuiltinCase[] = [
  ...["force", "force:"].map(token => commandCase(`${token.replace(":", "-colon")}-tool`, `/${token} read`, "Next turn forced to use read.", [send(`/${token}`, "Usage: /force:<tool-name> [prompt]")], [effect("builtin.session", "read", "/forcedTool")])),
  commandCase("force-prompt", "/force:read Inspect parity.txt", "PARITY_READ_COMPLETE", [], [effect("builtin.provider", ["read"], "/toolNames"), effect("builtin.provider", "parity\n", "/toolResults/0/content")]),
  commandCase("live-no-auth", "/live", "No Codex OAuth credential is available for a live call.", [], [effect("builtin.provider", 0, "/remoteRequests")]),
  commandCase("live-conversation", "/live", "PARITY_LIVE_REPLY", [
    { ...observed("muted"), terminalInput: " ", browserActions: [{ type: "click", selector: "[data-omp-role='omp.app.live'] button:has-text('Mute')" }] },
    { ...observed("listening"), terminalInput: " ", browserActions: [{ type: "click", selector: "[data-omp-role='omp.app.live'] button:has-text('Unmute')" }] },
    { ...observed("PARITY_LIVE_REPLY", [effect("builtin.live", true, "/ended")]), terminalInput: "\u001b", browserActions: [{ type: "click", selector: "[data-omp-role='omp.app.live'] button:has-text('End call')" }] },
    send("/retry", "Nothing to retry."),
  ], [effect("builtin.live", "PARITY_LIVE_USER", "/userTranscript"), effect("builtin.live", "PARITY_LIVE_REPLY", "/assistantTranscript"), effect("builtin.live", true, "/receivedAudio")]),
  commandCase("live-failure", "/live", "PARITY_LIVE_FAILURE", [send("/retry", "Nothing to retry.")], [effect("builtin.live", true, "/failed")]),
  commandCase("pause-resume", "/pause", "Paused — ready to suspend", [{
    ...observed("agents are running again.", [effect("builtin.session", false, "/paused")]),
    terminalInput: "\u001b", browserActions: escape,
  }], [effect("builtin.session", true, "/paused")]),
  commandCase("record-toggle", "/record", "Recording to", [send("/record", "replay: omp play · share: omp clip", [effect("builtin.recording", true, "/exists"), effect("builtin.recording", true, "/hasFrames"), effect("builtin.recording", false, "/active")])], [effect("builtin.recording", true, "/active")]),
  commandCase("forget-approvals", "/forget-approvals", "Remembered approvals forgotten; the next read or write asks again.", [], [effect("builtin.approvals", [])]),
  commandCase("loop-toggle", "/loop", "Loop mode enabled.", [send("/loop", "Loop mode disabled.", [effect("builtin.session", false, "/loopEnabled")])], [effect("builtin.session", true, "/loopEnabled")]),
  commandCase("queue-dispatch", "/queue Parity queued prompt", "PARITY_QUEUE_COMPLETE", [], [effect("builtin.provider", ["Parity queued prompt"], "/userPrompts")]),
  commandCase("queue-running", "/force read Parity blocked prompt", "PARITY_STREAM_STARTED", [
    send("/queue Parity follow-up", "Queued message for when the agent yields", [effect("builtin.session", ["Parity follow-up"], "/followUpQueue")]),
    { name: "Cancel active generation and restore queued draft", terminalInput: "\u001b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [
      { name: "Restored draft", terminal: { type: "screen", pattern: "(Parity follow-up)", group: 1 }, browser: { type: "dom", selector: composer, read: "value" }, expected: "Parity follow-up" },
      effect("builtin.session", [], "/followUpQueue"), effect("builtin.provider", true, "/cancelled"),
    ] },
  ]),
];
