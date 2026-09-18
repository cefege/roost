// Records the terminal startup card's stage transitions and its percent meter
// from inside the page, so specs can prove one smooth non-decreasing journey.
// Installed per page by the terminal smoke specs; reads only DOM attributes
// plus window.__smoke's stream probe.
//
// A split layout mounts one card per pane, so meter samples from different
// panes interleave — filter by sessionId when that matters.

import type { Page } from "@playwright/test";

type LoadingStageObservation = {
  stage: string;
  sessionId: string | null;
  recordedAtMs: number;
  stream: unknown | null;
  error: string | null;
};

type StartupMeterSample = {
  stage: string | null;
  percent: number;
  phase: string | null;
  sessionId: string | null;
};

type LoadingStageProbeWindow = Window & {
  __terminalLoadingStages?: string[];
  __terminalLoadingStageObservations?: LoadingStageObservation[];
  __terminalStartupMeter?: StartupMeterSample[];
  __terminalLoadingStageObserver?: MutationObserver;
  __smoke?: {
    terminalStreamProbe(sessionId: string): Promise<unknown>;
  };
};

function installLoadingStageProbeInPage(): void {
  const probeWindow = window as LoadingStageProbeWindow;
  const stages: string[] = [];
  const observations: LoadingStageObservation[] = [];
  const meter: StartupMeterSample[] = [];
  const recordedMeter = new WeakMap<Element, string>();
  const recordedStage = new WeakMap<Element, string>();
  const statusSelector = '[data-testid="terminal-loading-status"]';
  const captureStage = (status: Element, stage: string): void => {
    const sessionId = status.getAttribute("data-session-id");
    const observation: LoadingStageObservation = {
      stage,
      sessionId,
      recordedAtMs: performance.now(),
      stream: null,
      error: null,
    };
    observations.push(observation);
    if (!sessionId || !probeWindow.__smoke) return;
    void probeWindow.__smoke.terminalStreamProbe(sessionId)
      .then((stream) => { observation.stream = stream; })
      .catch((error: unknown) => { observation.error = String(error); });
  };
  const recordStage = (status: Element, stage: string | null) => {
    if (!stage || recordedStage.get(status) === stage) return;
    recordedStage.set(status, stage);
    stages.push(stage);
    captureStage(status, stage);
  };
  const recordStatus = (status: Element) => {
    recordStage(status, status.getAttribute("data-stage"));
  };
  // Current values only: reading oldValue here would drop the final
  // 100/complete mutation, which is the proof the journey finished.
  const recordMeter = (status: Element) => {
    const percent = Number(status.getAttribute("data-percent"));
    const phase = status.getAttribute("data-phase");
    const key = `${percent}|${phase}`;
    if (recordedMeter.get(status) === key) return;
    recordedMeter.set(status, key);
    meter.push({
      stage: status.getAttribute("data-stage"),
      percent: Number.isFinite(percent) ? percent : 0,
      phase,
      sessionId: status.getAttribute("data-session-id"),
    });
  };
  const recordTree = (node: Node) => {
    if (!(node instanceof Element)) return;
    if (node.matches(statusSelector)) {
      recordStatus(node);
      recordMeter(node);
    }
    for (const status of node.querySelectorAll(statusSelector)) {
      recordStatus(status);
      recordMeter(status);
    }
  };
  const observer = new MutationObserver((records) => {
    const changedStatuses = new Set<Element>();
    for (const record of records) {
      if (
        record.type === "attributes"
        && record.target instanceof Element
        && record.target.matches(statusSelector)
      ) {
        if (record.attributeName === "data-stage") {
          recordStage(record.target, record.oldValue);
        }
        changedStatuses.add(record.target);
      }
      for (const added of record.addedNodes) recordTree(added);
    }
    for (const status of changedStatuses) {
      recordStatus(status);
      recordMeter(status);
    }
  });
  observer.observe(document, {
    attributes: true,
    attributeFilter: ["data-stage", "data-percent", "data-phase"],
    attributeOldValue: true,
    childList: true,
    subtree: true,
  });
  probeWindow.__terminalLoadingStages = stages;
  probeWindow.__terminalLoadingStageObservations = observations;
  probeWindow.__terminalStartupMeter = meter;
  probeWindow.__terminalLoadingStageObserver = observer;
  for (const status of document.querySelectorAll(statusSelector)) {
    recordStatus(status);
    recordMeter(status);
  }
}

export async function installTerminalLoadingStageProbe(
  page: Page,
  nextDocument = false,
): Promise<void> {
  if (nextDocument) await page.addInitScript(installLoadingStageProbeInPage);
  else await page.evaluate(installLoadingStageProbeInPage);
}

export async function terminalLoadingStages(
  page: Page,
  disconnect = false,
): Promise<string[]> {
  return page.evaluate((shouldDisconnect) => {
    const probeWindow = window as LoadingStageProbeWindow;
    if (shouldDisconnect) probeWindow.__terminalLoadingStageObserver?.disconnect();
    return [...(probeWindow.__terminalLoadingStages ?? [])];
  }, disconnect);
}

export async function terminalLoadingStageObservations(
  page: Page,
  disconnect = false,
): Promise<LoadingStageObservation[]> {
  return page.evaluate((shouldDisconnect) => {
    const probeWindow = window as LoadingStageProbeWindow;
    if (shouldDisconnect) probeWindow.__terminalLoadingStageObserver?.disconnect();
    return structuredClone(probeWindow.__terminalLoadingStageObservations ?? []);
  }, disconnect);
}

export async function terminalStartupMeterSamples(
  page: Page,
  disconnect = false,
): Promise<StartupMeterSample[]> {
  return page.evaluate((shouldDisconnect) => {
    const probeWindow = window as LoadingStageProbeWindow;
    if (shouldDisconnect) probeWindow.__terminalLoadingStageObserver?.disconnect();
    return structuredClone(probeWindow.__terminalStartupMeter ?? []);
  }, disconnect);
}
