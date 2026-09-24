// Fleet peer floods start only after visible panes have geometric marker proof.
// Each trusted sample proves its active target is live at the rendered bottom before input.
// perf-fleet-probes supplies topology while scale-browser owns browser dispatch.
import type { PaintedMarkerProof } from "../../apps/web/src/smoke/smokeHarness.ts";
import type { RetainedMarkerScan } from "../../apps/web/src/smoke/smokeTypes.ts";
import {
  attachFleetFailure,
  captureFleetPresentation,
  type FleetPresentationCapture,
} from "./perf-fleet-diagnostics.ts";
import { expect } from "./fixtures.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import {
  scalePaneFocused,
  sendFixtureCommand,
  waitForPaintedScaleMarker,
  type ScaleDocument,
  type ScaleSession,
  type ScaleSmokeWindow,
} from "./terminal-scale-browser.ts";

const PEER_FLOOD_LINES = 384;

export type FleetFloodWorkload = "isolated" | "same_worker" | "other_worker" | "together";

export type FleetPeer = { document: ScaleDocument; session: ScaleSession };

type PreparedPeerFlood = {
  peer: FleetPeer;
  prefix: string;
  completionMarker: string;
  beforeDispatch: FleetPresentationCapture;
};

type ActivePeerFlood = { completion: Promise<PeerFloodProof> };

type PreparedFleetPeerFloods = {
  dispatch(): Promise<Array<Promise<PeerFloodProof>>>;
};

type ViewportMarkerScan = {
  min: number;
  max: number;
  missing: number;
  duplicated: number[];
  outOfOrder: number;
};

export type FleetFloodIntegrity = Pick<
  RetainedMarkerScan,
  | "pages" | "scrollbackTotal" | "retainedFloor" | "retainedFloorReason"
  | "markerMin" | "markerMax" | "markerMissing" | "markerDuplicated" | "markerOutOfOrder"
> & { viewport: ViewportMarkerScan };

export type PeerFloodProof = {
  sessionId: string;
  workerFp: string;
  floodDispatchMonotonicMs: number;
  completion: PaintedMarkerProof;
  integrity: FleetFloodIntegrity;
};

function peersFor(
  visible: readonly FleetPeer[],
  target: FleetPeer,
  workload: FleetFloodWorkload,
): FleetPeer[] {
  const peers = visible.filter((peer) => peer.session.id !== target.session.id);
  if (workload === "isolated") return [];
  if (workload === "same_worker") return peers.filter((peer) => peer.session.worker.workerFp === target.session.worker.workerFp);
  if (workload === "other_worker") return peers.filter((peer) => peer.session.worker.workerFp !== target.session.worker.workerFp);
  return peers;
}


export async function activateFleetTerminal(peer: FleetPeer): Promise<void> {
  const { page } = peer.document;
  const { id } = peer.session;
  const tab = page.getByTestId(`tab-${id}`);
  if (await tab.getAttribute("data-active") !== "true") {
    await tab.click();
    await expect(tab).toHaveAttribute("data-active", "true");
  }
  if (!await scalePaneFocused(page, id)) {
    await page.getByTestId(`terminal-slot-${id}`).click();
    await expect.poll(() => scalePaneFocused(page, id)).toBe(true);
  }
}

export async function expectFleetLiveTarget(target: FleetPeer): Promise<void> {
  const { page } = target.document;
  const { id } = target.session;
  await expect.poll(async () => {
    const [probe, render] = await Promise.all([
      readTerminalStreamProbe(page, id),
      page.evaluate((sessionId) => {
        const smokeWindow = window as unknown as ScaleSmokeWindow;
        return smokeWindow.__smoke.renderProbe(sessionId);
      }, id),
    ]);
    const presentation = probe.browser.presentation;
    const slot = probe.browser.slot;
    return slot.registered
      && slot.connected
      && slot.in_layout === true
      && slot.surface_active === true
      && slot.css_visible === true
      && probe.browser.visibility.document_visible
      && probe.browser.visibility.page_visible
      && presentation?.reader_intent === "live"
      && presentation.reader_reason === null
      && presentation.at_bottom
      && !presentation.hold_mask.selection
      && !presentation.hold_mask.link
      && probe.browser.reconcile_block_reason === null
      && render.atBottom;
  }).toBe(true);
}

export async function confirmFleetVisibleMarkers(peers: readonly FleetPeer[], runId: string): Promise<void> {
  await Promise.all(peers.map(async (peer, index) => {
    const marker = `FLEET-VISIBLE-${runId}-${index}`;
    await sendFixtureCommand(peer.document.page, peer.session.id, encodePtyFixtureCommand({ op: "EMIT", text: marker }));
    await waitForPaintedScaleMarker(peer.document.page, peer.session.id, marker);
  }));
}

async function scanViewportFloodTail(peer: FleetPeer, prefix: string): Promise<ViewportMarkerScan> {
  return peer.document.page.evaluate(({ id, markerPrefix }) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    const rows = slot?.querySelectorAll(".cell-viewport > .cell-row") ?? [];
    const pattern = new RegExp(`${markerPrefix.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(\\d+)`, "g");
    const counts = new Map<number, number>();
    const sequence: number[] = [];
    for (const row of rows) {
      pattern.lastIndex = 0;
      let match: RegExpExecArray | null;
      while ((match = pattern.exec(row.textContent ?? "")) !== null) {
        const marker = Number(match[1]);
        if (!Number.isSafeInteger(marker)) continue;
        counts.set(marker, (counts.get(marker) ?? 0) + 1);
        sequence.push(marker);
      }
    }
    const markers = [...counts.keys()];
    const min = markers.length === 0 ? 0 : Math.min(...markers);
    const max = markers.length === 0 ? 0 : Math.max(...markers);
    let missing = 0;
    for (let marker = min; marker <= max; marker++) if (!counts.has(marker)) missing++;
    const duplicated = markers.filter((marker) => (counts.get(marker) ?? 0) > 1).sort((left, right) => left - right);
    let outOfOrder = 0;
    for (let index = 1; index < sequence.length; index++) if (sequence[index]! < sequence[index - 1]!) outOfOrder++;
    return { min, max, missing, duplicated, outOfOrder };
  }, { id: peer.session.id, markerPrefix: prefix });
}

function expectCompleteFlood(
  retained: RetainedMarkerScan,
  viewport: ViewportMarkerScan,
  maximum: number,
  requireFirst: boolean,
): void {
  expect({
    missing: retained.markerMissing,
    duplicated: retained.markerDuplicated,
    outOfOrder: retained.markerOutOfOrder,
  }).toEqual({ missing: 0, duplicated: [], outOfOrder: 0 });
  if (requireFirst) expect(retained.markerMin).toBe(1);
  expect({
    minimum: viewport.min,
    maximum: viewport.max,
    missing: viewport.missing,
    duplicated: viewport.duplicated,
    outOfOrder: viewport.outOfOrder,
  }).toEqual({
    minimum: retained.markerMax + 1,
    maximum,
    missing: 0,
    duplicated: [],
    outOfOrder: 0,
  });
}

export async function verifyCompleteFleetFlood(
  peer: FleetPeer,
  prefix: string,
  maximum: number,
  requireFirst: boolean,
): Promise<FleetFloodIntegrity> {
  const [retained, viewport] = await Promise.all([
    peer.document.page.evaluate(({ id, markerPrefix }) => {
      const smokeWindow = window as unknown as ScaleSmokeWindow;
      return smokeWindow.__smoke.retainedMarkerScan(id, markerPrefix, 4_096);
    }, { id: peer.session.id, markerPrefix: prefix }),
    scanViewportFloodTail(peer, prefix),
  ]);
  expectCompleteFlood(retained, viewport, maximum, requireFirst);
  return {
    pages: retained.pages,
    scrollbackTotal: retained.scrollbackTotal,
    retainedFloor: retained.retainedFloor,
    retainedFloorReason: retained.retainedFloorReason,
    markerMin: retained.markerMin,
    markerMax: retained.markerMax,
    markerMissing: retained.markerMissing,
    markerDuplicated: retained.markerDuplicated,
    markerOutOfOrder: retained.markerOutOfOrder,
    viewport,
  };
}

async function dispatchPeerFlood(flood: PreparedPeerFlood): Promise<ActivePeerFlood> {
  const floodFrame = encodePtyFixtureCommand({
    op: "FLOOD",
    prefix: `\x1b[38;5;39m${flood.prefix}\x1b[0m`,
    count: PEER_FLOOD_LINES,
  });
  const completionFrame = encodePtyFixtureCommand({ op: "EMIT", text: flood.completionMarker });
  const floodDispatchMonotonicMs = await sendFixtureCommand(
    flood.peer.document.page,
    flood.peer.session.id,
    floodFrame + completionFrame,
  );
  const completion = (async (): Promise<PeerFloodProof> => {
    try {
      const completed = await waitForPaintedScaleMarker(
        flood.peer.document.page,
        flood.peer.session.id,
        flood.completionMarker,
      );
      const integrity = await verifyCompleteFleetFlood(
        flood.peer,
        flood.prefix,
        PEER_FLOOD_LINES,
        true,
      );
      return {
        sessionId: flood.peer.session.id,
        workerFp: flood.peer.session.worker.workerFp,
        floodDispatchMonotonicMs,
        completion: completed,
        integrity,
      };
    } catch (error) {
      await attachFleetFailure(
        flood.peer,
        flood.completionMarker,
        flood.beforeDispatch,
        "fleet-peer-failure.json",
      );
      throw error;
    }
  })();
  return { completion };
}

export async function prepareFleetPeerFloods(options: {
  visible: readonly FleetPeer[];
  unmounted: readonly ScaleSession[];
  target: FleetPeer;
  workload: FleetFloodWorkload;
  runId: string;
  sample: number;
}): Promise<PreparedFleetPeerFloods> {
  const peerFloods = await Promise.all(peersFor(options.visible, options.target, options.workload).map(async (peer, index) => ({
    peer,
    prefix: `FLEET-KEY-${options.runId}-${options.sample}-${index}-`,
    completionMarker: `FLEET-KEY-COMPLETE-${options.runId}-${options.sample}-${index}`,
    beforeDispatch: await captureFleetPresentation(peer),
  })));
  const producers = options.workload === "isolated"
    ? []
    : options.unmounted.filter((session) => options.workload === "together" || (options.workload === "same_worker"
      ? session.worker.workerFp === options.target.session.worker.workerFp
      : session.worker.workerFp !== options.target.session.worker.workerFp));
  return {
    async dispatch() {
      const [activeFloods] = await Promise.all([
        Promise.all(peerFloods.map((flood) => dispatchPeerFlood(flood))),
        Promise.all(producers.map((session, index) => sendFixtureCommand(
          options.target.document.page,
          session.id,
          encodePtyFixtureCommand({
            op: "FLOOD",
            prefix: `FLEET-PRODUCER-${options.runId}-${options.sample}-${index}-`,
            count: 64,
          }),
        ))),
      ]);
      return activeFloods.map((flood) => flood.completion);
    },
  };
}
