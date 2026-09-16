// The pane marker must track the transport a session is actually published
// over. The smoke tier can only observe steady state on each origin, so the
// transition — a grant landing or a socket dropping mid-session — is pinned
// here, against Solid's real client scheduler.

import { describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
// Type-only, so it is erased and cannot load the module before the mock lands.
import type { TerminalLocalTransport } from "../src/store/terminal-stream-transport.ts";

const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => ({ ...Solid }));

// Both imports must be dynamic: mock.module has to replace solid-js with its
// client build BEFORE the subject module resolves solid, or the effect never
// schedules. Same boundary trick as cellTerminalDocumentLifecycle.test.ts.

const transportModule = await import("../src/store/terminal-stream-transport.ts");
const indicator = await import("../src/store/local-transport-indicator.ts");

const SESSION = "11111111-2222-4333-8444-555555555555";

function fakeTransport(owned: Set<string>): TerminalLocalTransport {
  return {
    ownsSession: (sessionId: string) => owned.has(sessionId),
    noteViewPublished: () => {},
    generationToken: () => ({
      socketGeneration: 1,
      socketId: "socket-1",
      processEpoch: transportModule.LOCAL_TERMINAL_PROCESS_EPOCH,
      domainGeneration: 0n,
    }),
    publishView: () => true,
    publishResync: () => true,
    sendInput: () => ({ outcome: "rejected", reason: "test" }) as never,
    requestScrollback: () => Promise.reject(new Error("unused")),
    redial: () => true,
    reset: () => {},
  };
}

describe("sessionUsesLocalTransport", () => {
  test("repaints a pane when the local socket takes the session and gives it up", () => {
    indicator.installLocalTransportIndicator();
    const owned = new Set<string>();
    transportModule.registerTerminalLocalTransport(fakeTransport(owned));

    const seen: boolean[] = [];
    const dispose = Solid.createRoot((disposeRoot) => {
      Solid.createEffect(() => {
        seen.push(indicator.sessionUsesLocalTransport(SESSION));
      });
      return disposeRoot;
    });
    expect(seen).toEqual([false]);

    // A grant landing mid-session must repaint the marker rather than leave the
    // pane reporting whatever transport it had at mount.
    owned.add(SESSION);
    transportModule.notifyTerminalLocalTransportChanged();
    expect(seen).toEqual([false, true]);

    // ...and the socket dropping must clear it, or the pane keeps claiming a
    // fast path whose frames now travel through the coordinator.
    owned.delete(SESSION);
    transportModule.notifyTerminalLocalTransportChanged();
    expect(seen).toEqual([false, true, false]);

    dispose();
  });
});
