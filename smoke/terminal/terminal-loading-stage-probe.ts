import type { Page } from "@playwright/test";

type LoadingStageProbeWindow = Window & {
  __terminalLoadingStages?: string[];
  __terminalLoadingStageObserver?: MutationObserver;
};

function installLoadingStageProbeInPage(): void {
  const probeWindow = window as LoadingStageProbeWindow;
  const stages: string[] = [];
  const recordedStage = new WeakMap<Element, string>();
  const statusSelector = '[data-testid="terminal-loading-status"]';
  const recordStage = (status: Element, stage: string | null) => {
    if (!stage || recordedStage.get(status) === stage) return;
    recordedStage.set(status, stage);
    stages.push(stage);
  };
  const recordStatus = (status: Element) => {
    recordStage(status, status.getAttribute("data-stage"));
  };
  const recordTree = (node: Node) => {
    if (!(node instanceof Element)) return;
    if (node.matches(statusSelector)) recordStatus(node);
    for (const status of node.querySelectorAll(statusSelector)) recordStatus(status);
  };
  const observer = new MutationObserver((records) => {
    const changedStatuses = new Set<Element>();
    for (const record of records) {
      if (
        record.type === "attributes"
        && record.target instanceof Element
        && record.target.matches(statusSelector)
      ) {
        recordStage(record.target, record.oldValue);
        changedStatuses.add(record.target);
      }
      for (const added of record.addedNodes) recordTree(added);
    }
    for (const status of changedStatuses) recordStatus(status);
  });
  observer.observe(document, {
    attributes: true,
    attributeFilter: ["data-stage"],
    attributeOldValue: true,
    childList: true,
    subtree: true,
  });
  probeWindow.__terminalLoadingStages = stages;
  probeWindow.__terminalLoadingStageObserver = observer;
  for (const status of document.querySelectorAll(statusSelector)) recordStatus(status);
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
