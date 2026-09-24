// Guards coordinator cache residency accounting for viewport-only checkpoints.
// Older workers may send history-bearing full frames; validation runs before
// normalization, but ResidentCache must charge only the final viewport.
import { expect, test } from "bun:test";
import {
  SESSION,
  STREAM,
  deltaFrame,
  fullFrame,
  makeHarness,
  row,
} from "./terminal-screen-hub-harness.ts";

interface TerminalScreenHubInternals {
  readonly sessions: Map<string, {
    cache: { rows: number; spans: number } | null;
  }>;
}

test("full and live history never enter cache residency accounting", () => {
  const { hub } = makeHarness();
  hub.expectStream(SESSION, STREAM, 8, 2);

  const legacy = fullFrame();
  const historyRows = Array.from({ length: 300 }, (_, index) => row(index, `history-${index}`));
  legacy.scrollbackRows = historyRows;
  legacy.scrollbackTotal = BigInt(historyRows.length);
  legacy.sbBase = 0n;
  hub.publishFrame(SESSION, legacy);
  const internals = hub as unknown as TerminalScreenHubInternals;
  expect(internals.sessions.get(SESSION)?.cache).toMatchObject({ rows: 2, spans: 2 });
  const delta = deltaFrame();
  const appendedRows = Array.from({ length: 300 }, (_, index) => row(
    historyRows.length + index,
    `append-${index}`,
  ));
  delta.scrollbackAppend = appendedRows;
  delta.scrollbackTotal = BigInt(historyRows.length + appendedRows.length);
  hub.publishFrame(SESSION, delta);
  expect(hub.snapshot(SESSION)).toMatchObject({ seq: 2, valid: true });
  expect(internals.sessions.get(SESSION)?.cache).toMatchObject({ rows: 2, spans: 2 });
});
