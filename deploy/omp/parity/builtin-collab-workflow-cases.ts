import type { BuiltinCase } from "./scenario";
import { commandCase, effect, send } from "./builtin-steps";

export const collabWorkflowCases: BuiltinCase[] = [
  {
    ...commandCase("collab-host-room-lifecycle", "/collab view", "Collab session started!", [
      send("/collab status", "Collab:", [effect("builtin.collab", true, "/relayConnected"), effect("builtin.collab", 1, "/participants")]),
      send("/collab list", "1 active local Collab host", [effect("builtin.collab", 1, "/registryHosts"), effect("builtin.collab", true, "/registryMatchesCurrent")]),
      send("/collab view", "Read-only collab session active", [effect("builtin.collab", 1, "/hostGeneration"), effect("builtin.collab", "view", "/hostAccess")]),
      send("/collab start", "Collab session restarted with control access", [effect("builtin.collab", 2, "/hostGeneration"), effect("builtin.collab", "control", "/hostAccess")]),
      send("/collab start", "Collab session active", [effect("builtin.collab", 2, "/hostGeneration")]),
      send("/leave", "Collab stopped", [effect("builtin.collab", false, "/hostActive"), effect("builtin.collab", 0, "/registryHosts")]),
      send("/collab", "Collab session started!", [effect("builtin.collab", 3, "/hostGeneration"), effect("builtin.collab", "control", "/hostAccess")]),
      send("/collab stop", "Collab stopped", [effect("builtin.collab", false, "/hostActive"), effect("builtin.collab", 0, "/registryHosts")]),
      send("/collab list", "No active Collab hosts", [effect("builtin.collab", 0, "/registryHosts")]),
      send("/collab start", "Collab session started!", [effect("builtin.collab", 4, "/hostGeneration")]),
      send("/new", "New session started", [effect("builtin.collab", false, "/hostActive"), effect("builtin.collab", 0, "/registryHosts")]),
      send("/collab status", "Not in a collab session"),
    ], [effect("builtin.collab", true, "/hostActive"), effect("builtin.collab", "view", "/hostAccess"), effect("builtin.collab", 1, "/hostGeneration"), effect("builtin.collab", true, "/registryMatchesCurrent")]),
    settings: { "collab.autoStart": "off" },
  },
  {
    ...commandCase("collab-guest-control-write-restore", "/join PARITY_COLLAB_JOIN_LINK", "Joined collab session", [
      send("/collab status", "In a collab session as a guest (/leave to exit)", [effect("builtin.collab", false, "/guestReadOnly"), effect("builtin.collab", "Parity collab peer", "/guestState/sessionName")]),
      send("Parity write collab-control.txt then confirm.", "PARITY_COLLAB_CONTROL_DONE", [effect("builtin.workspace-file", "PARITY_COLLAB_CONTROL_FILE\n", "/collab-control.txt/content"), effect("builtin.provider", 2, "/requestCount")]),
      send("/collab", "Already in a collab session as a guest (/leave first)"),
      send("/join PARITY_COLLAB_JOIN_LINK", "Already in a collab session (/leave first)"),
      send("/leave", "PARITY_LOCAL_COLLAB_ANSWER", [effect("builtin.collab", false, "/guestActive"), effect("builtin.collab", true, "/localSessionRestored"), effect("builtin.session", ["PARITY_LOCAL_COLLAB_QUESTION"], "/diskUserTexts")]),
      send("/collab status", "Not in a collab session"),
    ], [effect("builtin.collab", true, "/guestActive"), effect("builtin.collab", true, "/guestWelcomed"), effect("builtin.collab", false, "/guestReadOnly")]),
    settings: { "collab.autoStart": "off" },
  },
  {
    ...commandCase("collab-guest-view-readonly-restore", "/join PARITY_COLLAB_JOIN_LINK", "Joined collab session (read-only)", [
      send("/collab status", "In a collab session as a read-only guest (/leave to exit)", [effect("builtin.collab", true, "/guestReadOnly"), effect("builtin.collab", "Parity collab peer", "/guestState/sessionName")]),
      send("Parity forbidden guest prompt must not write collab-control.txt.", "This collab link is read-only", [effect("builtin.provider", 0, "/requestCount"), effect("builtin.workspace-file", false, "/collab-control.txt/exists"), effect("builtin.session", 2, "/messageCount")]),
      send("/leave", "PARITY_LOCAL_COLLAB_ANSWER", [effect("builtin.collab", false, "/guestActive"), effect("builtin.collab", true, "/localSessionRestored"), effect("builtin.session", ["PARITY_LOCAL_COLLAB_QUESTION"], "/diskUserTexts")]),
      send("/collab status", "Not in a collab session"),
    ], [effect("builtin.collab", true, "/guestActive"), effect("builtin.collab", true, "/guestWelcomed"), effect("builtin.collab", true, "/guestReadOnly")]),
    settings: { "collab.autoStart": "off" },
  },
  {
    ...commandCase("collab-qr-emergency-row", "/collab", "Collab session started!", [
      {
        name: "Both renderers survive a transcript viewport smaller than the invite QR",
        terminalResize: { columns: 60, rows: 7 },
        browserActions: [{ type: "resize", width: 480, height: 420 }],
        comparisons: [effect("builtin.collab", true, "/hostActive")],
      },
      {
        name: "Both renderers return to a full viewport",
        terminalResize: { columns: 120, rows: 48 },
        browserActions: [{ type: "resize", width: 1200, height: 960 }],
        comparisons: [effect("builtin.collab", true, "/hostActive")],
      },
      send("/collab stop", "Collab stopped", [effect("builtin.collab", false, "/hostActive")]),
    ], [effect("builtin.collab", true, "/hostActive")]),
    covers: ["component:packages/tui/src/chrome/collab-qrcode.ts#CollabQrCodeComponent"],
    settings: { "collab.autoStart": "off" },
  },
];
