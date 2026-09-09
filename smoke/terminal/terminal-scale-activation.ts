// Paces fresh marker-driven terminal activation across a bounded document set.
// PR and soak harnesses use it after they have reserved worker capacity.
// A session reaches an accepted baseline before the input protocol emits its marker.
// Browser/session primitives remain in terminal-scale-browser.ts.

import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import {
  navigateAndPaint,
  runPacedBatches,
  sendFixtureCommand,
  waitForScaleCondition,
  type ScaleSession,
  type ScaleSlot,
  type ScaleSmokeWindow,
  SCALE_ACTIVATION_PACE_MS,
  SCALE_BATCH_SIZE,
  SCALE_MARKER_TIMEOUT_MS,
} from "./terminal-scale-browser.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";

export async function activateSlots(
  slots: ScaleSlot[],
  sessions: readonly ScaleSession[],
  offset: number,
  concurrency = SCALE_BATCH_SIZE,
): Promise<void> {
  await runPacedBatches(slots, concurrency, SCALE_ACTIVATION_PACE_MS, async (slot, index) => {
    const session = sessions[(offset + index) % sessions.length]!;
    slot.session = session;
    await slot.document.page.evaluate((sessionId) => {
      const smokeWindow = window as unknown as ScaleSmokeWindow;
      smokeWindow.__smoke.navigate(`/s/${sessionId}`);
    }, session.id);
    await waitForScaleCondition(`terminal baseline ${session.id}`, SCALE_MARKER_TIMEOUT_MS, async () => {
      const probe = await readTerminalStreamProbe(slot.document.page, session.id);
      return probe.browser.view.active && probe.browser.replica.baseline_ready;
    });
    const activationMarker = `SCALE-ACTIVATE-${crypto.randomUUID()}`;
    await sendFixtureCommand(slot.document.page, session.id, encodePtyFixtureCommand({
      op: "EMIT",
      text: activationMarker,
    }));
    await navigateAndPaint(slot.document.page, session.id, activationMarker);
  });
}
