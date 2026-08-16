import { expect, mock, test } from "bun:test";
import { asChannelId, asSessionId, ClientControlFrame } from "@roost/shared/wire";
import { handleSpawnShell } from "../src/browser-command-spawn.ts";
import type { SessionManager } from "../src/session-manager.ts";
import type { CoordLink } from "../src/transport/CoordLink.ts";

const SID = asSessionId("00000000-0000-4000-8000-000000000818");

test("spawn command installs the authenticated viewer preclaim and reports its effective sequence", async () => {
  const send = mock(() => ({ kind: "sent" as const }));
  const spawnShell = mock(async () => ({
    sessionId: SID,
    channelId: asChannelId(23),
  }));
  const frame = ClientControlFrame.parse({
    kind: "spawn-shell",
    folder: "/work",
    cols: 111,
    rows: 39,
    session_id: SID,
    preclaim_initial_viewport: true,
    initial_viewport_client_seq: 12,
  });
  if (frame.kind !== "spawn-shell") throw new Error("spawn-shell fixture did not narrow");

  handleSpawnShell(frame, "browser:tab-a", "request-1", {
    coordLink: { send } as unknown as CoordLink,
    sessionMgr: { spawnShell } as unknown as SessionManager,
  });
  await Promise.resolve();

  expect(spawnShell).toHaveBeenCalledWith(
    "/work",
    111,
    39,
    SID,
    {
      viewerId: "browser:tab-a",
      clientSeq: 12n,
      cols: 111,
      rows: 39,
    },
  );
  expect(send).toHaveBeenLastCalledWith({
    kind: "rpc-ok",
    request_id: "request-1",
    data: {
      session_id: SID,
      channel_id: 23,
      initial_viewport_preclaimed: true,
      effective_client_seq: 12,
    },
  });
});
