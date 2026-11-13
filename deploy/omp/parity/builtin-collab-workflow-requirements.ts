import type { BuiltinRequirements } from "./builtin-requirements";

export interface CollabWorkflowProfile {
  evidenceClass: "canonical-collaboration-through-owned-relay";
  relay: {
    module: "packages/collab-web/scripts/local-relay.ts";
    factory: "startLocalRelay";
    port: 0;
    options: { hostname: "127.0.0.1" };
    binding: "owned-loopback-only";
  };
  peer?: {
    launch: "same-real-source-interactive-tui-profile";
    home: "separate-owned-peer-home";
    workspace: "same-owned-workspace";
    session: { title: "Parity collab peer"; userText: "PARITY_COLLAB_HOST_QUESTION"; assistantText: "PARITY_COLLAB_HOST_ANSWER" };
    startupCommand: "/collab start" | "/collab view";
    dispatch: "actual-editor-and-builtin-dispatcher";
    ready: "actual-startupComplete-and-controller-idle-and-relayConnected";
    binding: { token: "PARITY_COLLAB_JOIN_LINK"; source: "peer.ctx.collabController.host.link" | "peer.ctx.collabController.host.viewLink"; phase: "before-initial-command" };
    cleanupCommands: ["/collab stop", "/exit"];
  };
}

export const collabWorkflowCaptureSource = "Observe the current actual interactive-process ctx.collabController/ctx.collabGuest and production listCollabHosts(): hostActive,hostAccess,hostGeneration,relayConnected,participants,registryHosts,registryMatchesCurrent,guestActive,guestReadOnly,guestWelcomed,guestState. localSessionRestored compares actual current saved file with original manifest.sessionPath after guest restoration. Read actual host public getters and guest.state; no invented room/participant/registry state. Capture only requested CURRENT-checkpoint pointers and await real controller.idle where appropriate.";

export function applyCollabWorkflowRequirements(requirements: Record<string, BuiltinRequirements>): void {
  for (const [id, peerMode] of [
    ["builtin-collab-host-room-lifecycle", null],
    ["builtin-collab-qr-emergency-row", null],
    ["builtin-collab-guest-control-write-restore", "control"],
    ["builtin-collab-guest-view-readonly-restore", "view"],
  ] as const) {
    const requirement = requirements[id];
    if (!requirement) throw new Error(`Missing canonical collaboration workflow ${id}`);
    requirement.session.seed = "transcript";
    requirement.session.userText = "PARITY_LOCAL_COLLAB_QUESTION";
    requirement.session.assistantText = "PARITY_LOCAL_COLLAB_ANSWER";
    requirement.services = [{ kind: "collab-relay", behavior: "unchanged source local-relay binary forwarding; actual end-to-end source encryption/host/guest controllers", input: { mode: "canonical" } }];
    requirement.collabWorkflow = {
      evidenceClass: "canonical-collaboration-through-owned-relay",
      relay: { module: "packages/collab-web/scripts/local-relay.ts", factory: "startLocalRelay", port: 0, options: { hostname: "127.0.0.1" }, binding: "owned-loopback-only" },
    };
    if (peerMode !== null) requirement.collabWorkflow.peer = {
      launch: "same-real-source-interactive-tui-profile", home: "separate-owned-peer-home", workspace: "same-owned-workspace",
      session: { title: "Parity collab peer", userText: "PARITY_COLLAB_HOST_QUESTION", assistantText: "PARITY_COLLAB_HOST_ANSWER" },
      startupCommand: peerMode === "control" ? "/collab start" : "/collab view",
      dispatch: "actual-editor-and-builtin-dispatcher", ready: "actual-startupComplete-and-controller-idle-and-relayConnected",
      binding: { token: "PARITY_COLLAB_JOIN_LINK", source: peerMode === "control" ? "peer.ctx.collabController.host.link" : "peer.ctx.collabController.host.viewLink", phase: "before-initial-command" },
      cleanupCommands: ["/collab stop", "/exit"],
    };
    requirement.prerequisites.push(
      "Reuse the actual source packages/collab-web/scripts/local-relay.ts startLocalRelay(0,{hostname:'127.0.0.1'}), not a fixture protocol reimplementation, fabricated encrypted frame or fake peer. Verify its returned actual hostname/port and use its awaited idempotent stop(). Inject its actual relay URL through collab.relayUrl before source controllers start; collab.webUrl=https://parity-collab.invalid is display-only and must never navigate externally.",
      "Positive guest cases require a SECOND REAL owned source interactive TUI process, real AgentSession/SessionManager/CollabController/CollabHost, distinct fresh peer HOME/session registry and same owned workspace/provider. Dispatch its startup command through its actual editor after canonical interactive startup. Before initial /join bind PARITY_COLLAB_JOIN_LINK from that live peer host's actual control/view link, per leg; never share room/key/IDs between legs or synthesize a link/replica/welcome. Main process uses unchanged CollabGuestLink and persists/restores its actual session files.",
      "On cleanup send real peer /collab stop then /exit through its editor and supervise actual process termination; await canonical registry withdrawal and stop the actual relay. A missing peer supervisor, live link binding, loopback fence or guest state capture is a hard missing prerequisite, not an excluded command or successful generic fallback.",
    );
    for (const capture of requirement.captures) if (capture.channel === "builtin.collab") capture.source = collabWorkflowCaptureSource;
  }
  requirements["builtin-collab-guest-control-write-restore"].models.turns = [
    { match: { modelId: "parity-coder", textIncludes: "Parity write collab-control.txt then confirm.", purpose: "foreground" }, toolCalls: [{ id: "parity-collab-control-write", name: "write", arguments: { path: "collab-control.txt", content: "PARITY_COLLAB_CONTROL_FILE\n" } }] },
    { match: { modelId: "parity-coder", textIncludes: "Parity write collab-control.txt then confirm.", purpose: "foreground" }, text: "PARITY_COLLAB_CONTROL_DONE" },
  ];
}
