// Fleet reader diagnostics capture the browser event ordering around a held paint.
// Fleet performance probes install this test-only observer before workload dispatch.
// It reads the existing smoke probe and native grid geometry without affecting renderer state.
import type { SmokeApi, TerminalStreamProbe } from "../../apps/web/src/smoke/smokeTypes.ts";
import { test } from "./fixtures.ts";
import type { FleetPeer } from "./perf-fleet-peer-flood.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";

export interface FleetTransportCapture {
  active_kind: "sync" | "loopback" | "webrtc" | null;
  active_candidate_type: "host" | "srflx" | "prflx" | "none" | null;
  active_worker_control_rtt_ms: number | null;
  pending_input_count: number | null;
}

export interface FleetPresentationCapture {
  probe: TerminalStreamProbe | null;
  geometry: {
    capturedMonotonicMs: number;
    scrollTop: number;
    scrollHeight: number;
    clientHeight: number;
    fromBottom: number;
    selection: {
      rangeCount: number;
      collapsed: boolean;
      anchorInPane: boolean;
      focusInPane: boolean;
    } | null;
  } | null;
  transport: FleetTransportCapture | null;
  diagnosticError: string | null;
}

type FleetReaderTraceEntry = Record<string, unknown>;

type FleetReaderTrace = {
  entries: Array<FleetReaderTraceEntry | undefined>;
  head: number;
  size: number;
  droppedEvents: number;
  disposer: () => void;
};

type FleetReaderTraceWindow = Window & { __fleetReaderTrace?: FleetReaderTrace };

const FLEET_READER_TRACE_CAPACITY = 512;

function captureFleetTransport(probe: TerminalStreamProbe | null): FleetTransportCapture | null {
  if (!probe) return null;
  const route = probe.browser.route;
  return {
    active_kind: route.active?.kind ?? null,
    active_candidate_type: route.active?.candidate_type ?? null,
    active_worker_control_rtt_ms: route.active?.worker_control_rtt_ms ?? null,
    pending_input_count: route.pending_input_count,
  };
}

function diagnosticMessage(label: string, error: unknown): string {
  return `${label}: ${String(error)}`;
}

export async function captureFleetPresentation(peer: FleetPeer): Promise<FleetPresentationCapture> {
  const [probeResult, geometryResult] = await Promise.all([
    readTerminalStreamProbe(peer.document.page, peer.session.id)
      .then((probe) => ({ probe, error: null as string | null }))
      .catch((error) => ({ probe: null, error: diagnosticMessage("probe", error) })),
    peer.document.page.evaluate((sessionId) => {
      const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(sessionId)}"]`);
      const grid = slot?.querySelector(".cell-grid");
      if (!(slot instanceof HTMLElement) || !(grid instanceof HTMLElement)) return null;
      const selection = window.getSelection();
      const rangeCount = selection?.rangeCount ?? 0;
      return {
        capturedMonotonicMs: performance.now(),
        scrollTop: grid.scrollTop,
        scrollHeight: grid.scrollHeight,
        clientHeight: grid.clientHeight,
        fromBottom: grid.scrollHeight - grid.clientHeight - grid.scrollTop,
        selection: selection ? {
          rangeCount,
          collapsed: selection.isCollapsed,
          anchorInPane: !!selection.anchorNode && slot.contains(selection.anchorNode),
          focusInPane: !!selection.focusNode && slot.contains(selection.focusNode),
        } : null,
      };
    }, peer.session.id)
      .then((geometry) => ({ geometry, error: null as string | null }))
      .catch((error) => ({ geometry: null, error: diagnosticMessage("geometry", error) })),
  ]);
  return {
    probe: probeResult.probe,
    geometry: geometryResult.geometry,
    transport: captureFleetTransport(probeResult.probe),
    diagnosticError: [probeResult.error, geometryResult.error].filter((message): message is string => message !== null).join("; ") || null,
  };
}

export async function installFleetReaderTrace(peers: readonly FleetPeer[]): Promise<void> {
  const peersByPage = new Map<FleetPeer["document"]["page"], FleetPeer[]>();
  for (const peer of peers) {
    const pagePeers = peersByPage.get(peer.document.page) ?? [];
    pagePeers.push(peer);
    peersByPage.set(peer.document.page, pagePeers);
  }
  await Promise.all([...peersByPage].map(async ([page, pagePeers]) => {
    await page.evaluate(({ sessionIds, capacity }) => {
      const traceWindow = window as FleetReaderTraceWindow;
      traceWindow.__fleetReaderTrace?.disposer();
      const trace: FleetReaderTrace = {
        entries: Array<FleetReaderTraceEntry | undefined>(capacity),
        head: 0,
        size: 0,
        droppedEvents: 0,
        disposer: () => undefined,
      };
      const selectionMetadata = (slot: Element) => {
        const selection = window.getSelection();
        if (!selection) return null;
        return {
          rangeCount: selection.rangeCount,
          collapsed: selection.isCollapsed,
          anchorInPane: !!selection.anchorNode && slot.contains(selection.anchorNode),
          focusInPane: !!selection.focusNode && slot.contains(selection.focusNode),
        };
      };
      const gridMetadata = (grid: Element | null) => grid instanceof HTMLElement && grid.isConnected
        ? {
            available: true,
            scrollTop: grid.scrollTop,
            scrollHeight: grid.scrollHeight,
            clientHeight: grid.clientHeight,
            fromBottom: grid.scrollHeight - grid.clientHeight - grid.scrollTop,
          }
        : { available: false };
      const presentationMetadata = (sessionId: string) => {
        const smokeWindow = window as unknown as Pick<Window, never> & { __smoke: SmokeApi };
        const snapshot = smokeWindow.__smoke.terminalBrowserSnapshot(sessionId);
        const presentation = snapshot.presentation;
        const route = snapshot.route;
        return presentation ? {
          reader_intent: presentation.reader_intent,
          reader_reason: presentation.reader_reason,
          at_bottom: presentation.at_bottom,
          hold_mask: presentation.hold_mask,
          canonical: presentation.canonical,
          reconciled: presentation.reconciled,
          reconcile_block_reason: snapshot.reconcile_block_reason,
          transport: {
            kind: route.active?.kind ?? null,
            candidate_type: route.active?.candidate_type ?? null,
            worker_control_rtt_ms: route.active?.worker_control_rtt_ms ?? null,
            pending_input_count: route.pending_input_count,
          },
        } : null;
      };
      const paneForNode = (node: EventTarget | null) => {
        const element = node instanceof Element ? node : node instanceof Node ? node.parentElement : null;
        for (const sessionId of sessionIds) {
          const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(sessionId)}"]`);
          if (slot && element && slot.contains(element)) return { sessionId, slot, grid: slot.querySelector(".cell-grid") };
        }
        return null;
      };
      const record = (entry: FleetReaderTraceEntry) => {
        const writeIndex = (trace.head + trace.size) % capacity;
        if (trace.size === capacity) {
          trace.entries[trace.head] = entry;
          trace.head = (trace.head + 1) % capacity;
          trace.droppedEvents += 1;
        } else {
          trace.entries[writeIndex] = entry;
          trace.size += 1;
        }
      };
      const recordPaneEvent = (event: Event, stage: "ingress" | "after_handlers") => {
        const pane = paneForNode(event.target);
        if (!pane) return;
        const wheel = event instanceof WheelEvent ? event.deltaY : null;
        const key = event instanceof KeyboardEvent ? event.key : null;
        record({
          stage,
          type: event.type,
          monotonicMs: performance.now(),
          sessionId: pane.sessionId,
          isTrusted: event.isTrusted,
          geometry: gridMetadata(pane.grid),
          selection: selectionMetadata(pane.slot),
          wheelDirection: wheel === null ? null : wheel === 0 ? "none" : wheel < 0 ? "up" : "down",
          presentation: stage === "after_handlers" && event.type === "scroll"
            ? presentationMetadata(pane.sessionId)
            : null,
          scrollNavigationKey: key === null ? null : ["ArrowDown", "ArrowUp", "PageDown", "PageUp", "Home", "End", " "].includes(key),
        });
      };
      const ingress = (event: Event) => recordPaneEvent(event, "ingress");
      const recordResize = (event: Event) => {
        for (const sessionId of sessionIds) {
          const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(sessionId)}"]`);
          const grid = slot?.querySelector(".cell-grid") ?? null;
          record({ stage: "ingress", type: "resize", monotonicMs: performance.now(), sessionId, isTrusted: event.isTrusted, geometry: gridMetadata(grid), selection: slot ? selectionMetadata(slot) : null, wheelDirection: null, scrollNavigationKey: null });
        }
      };
      const lastSelectedSessionIds = new Set<string>();
      const recordSelection = (event: Event) => {
        const selection = window.getSelection();
        const selectedSessionIds = new Set<string>();
        for (const sessionId of sessionIds) {
          const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(sessionId)}"]`);
          if (!slot || (!selection?.anchorNode || !slot.contains(selection.anchorNode)) && (!selection?.focusNode || !slot.contains(selection.focusNode))) continue;
          selectedSessionIds.add(sessionId);
          lastSelectedSessionIds.add(sessionId);
          record({ stage: "ingress", type: "selectionchange", monotonicMs: performance.now(), sessionId, isTrusted: event.isTrusted, geometry: gridMetadata(slot.querySelector(".cell-grid")), selection: selectionMetadata(slot), wheelDirection: null, scrollNavigationKey: null });
        }
        for (const sessionId of lastSelectedSessionIds) {
          if (selectedSessionIds.has(sessionId)) continue;
          const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(sessionId)}"]`);
          record({ stage: "ingress", type: "selectionchange", monotonicMs: performance.now(), sessionId, isTrusted: event.isTrusted, geometry: gridMetadata(slot?.querySelector(".cell-grid") ?? null), selection: slot ? selectionMetadata(slot) : null, wheelDirection: null, scrollNavigationKey: null });
          lastSelectedSessionIds.delete(sessionId);
        }
      };
      const eventTypes: Array<"scroll" | "wheel" | "touchmove" | "pointerdown" | "pointerup" | "keydown"> = ["scroll", "wheel", "touchmove", "pointerdown", "pointerup", "keydown"];
      for (const eventType of eventTypes) document.addEventListener(eventType, ingress, { capture: true, passive: true });
      document.addEventListener("selectionchange", recordSelection, { passive: true });
      window.addEventListener("resize", recordResize, { passive: true });
      const afterScrollListeners: Array<{ grid: HTMLElement; listener: EventListener }> = [];
      for (const sessionId of sessionIds) {
        const grid = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(sessionId)}"] .cell-grid`);
        if (!(grid instanceof HTMLElement)) continue;
        const listener = (event: Event) => recordPaneEvent(event, "after_handlers");
        grid.addEventListener("scroll", listener, { passive: true });
        afterScrollListeners.push({ grid, listener });
      }
      trace.disposer = () => {
        for (const eventType of eventTypes) document.removeEventListener(eventType, ingress, { capture: true });
        document.removeEventListener("selectionchange", recordSelection);
        window.removeEventListener("resize", recordResize);
        for (const { grid, listener } of afterScrollListeners) grid.removeEventListener("scroll", listener);
        if (traceWindow.__fleetReaderTrace === trace) delete traceWindow.__fleetReaderTrace;
      };
      traceWindow.__fleetReaderTrace = trace;
    }, { sessionIds: pagePeers.map((peer) => peer.session.id), capacity: FLEET_READER_TRACE_CAPACITY });
  }));
}

export async function disposeFleetReaderTrace(peers: readonly FleetPeer[]): Promise<void> {
  const pages = [...new Set(peers.map((peer) => peer.document.page))];
  await Promise.all(pages.map((page) => page.evaluate(() => {
    (window as FleetReaderTraceWindow).__fleetReaderTrace?.disposer();
  }).catch(() => undefined)));
}

export async function attachFleetFailure(
  peer: FleetPeer,
  completionMarker: string,
  beforeDispatch: FleetPresentationCapture,
  attachmentName: "fleet-peer-failure.json" | "fleet-drain-failure.json",
): Promise<void> {
  try {
    const [atFailure, trace] = await Promise.all([
      captureFleetPresentation(peer),
      peer.document.page.evaluate(() => {
        const trace = (window as FleetReaderTraceWindow).__fleetReaderTrace;
        if (!trace) return { events: [], capacity: 0, droppedEvents: 0, diagnosticError: null };
        const events: FleetReaderTraceEntry[] = [];
        for (let index = 0; index < trace.size; index += 1) {
          const entry = trace.entries[(trace.head + index) % trace.entries.length];
          if (entry) events.push(entry);
        }
        return { events, capacity: trace.entries.length, droppedEvents: trace.droppedEvents, diagnosticError: null };
      }).catch((error) => ({ events: [], capacity: 0, droppedEvents: 0, diagnosticError: diagnosticMessage("trace", error) })),
    ]);
    await test.info().attach(attachmentName, {
      body: JSON.stringify({
        sessionId: peer.session.id,
        workerFp: peer.session.worker.workerFp,
        completionMarker,
        beforeDispatch,
        atFailure,
        events: trace.events,
        droppedEvents: trace.droppedEvents,
        eventCapacity: trace.capacity,
        traceComplete: trace.capacity > 0 && trace.diagnosticError === null && trace.droppedEvents === 0,
        diagnosticError: trace.diagnosticError ?? null,
      }, (_, item) => typeof item === "bigint" ? item.toString() : item, 2),
      contentType: "application/json",
    });
  } catch {}
}
