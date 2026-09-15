// Smoke-only terminal DOM fault — smoke.ts alone composes this controller into window.__smoke.
// It freezes renderer DOM reconciliation for one registered owner while canonical frames still apply.
// Current Sync-generation changes, renderer retirement, and explicit release restore exact renderer methods.

import {
  currentSyncV2TerminalState,
  registerSyncV2GenerationHandler,
  type SyncV2TerminalState,
} from "../store/sync.ts";
import { rendererRegistryEntry, type RendererRegistryEntry } from "./terminalPreview.ts";

export interface SmokeTerminalDomFaultMethods {
  holdTerminalDomForCurrentGeneration(sessionId: string): void;
  releaseTerminalDomHold(sessionId: string): void;
}

type RendererDomMethod = (...args: unknown[]) => unknown;

type RendererDomMethods = {
  renderFull: RendererDomMethod;
  renderViewportRepair: RendererDomMethod;
  renderDelta: RendererDomMethod;
  _markReconciledIfCurrent: RendererDomMethod;
};

type RendererDomMethodOriginals = {
  renderFull: RendererDomMethod;
  renderFullWasOwn: boolean;
  renderViewportRepair: RendererDomMethod;
  renderViewportRepairWasOwn: boolean;
  renderDelta: RendererDomMethod;
  renderDeltaWasOwn: boolean;
  markReconciledIfCurrent: RendererDomMethod;
  markReconciledIfCurrentWasOwn: boolean;
};

type TerminalGenerationIdentity = {
  socketGeneration: number;
  socketId: string;
  processEpoch: string;
  domainGeneration: bigint;
};

type TerminalDomRetirementFrame = number;

type TerminalDomHold = {
  sessionId: string;
  rendererEntry: RendererRegistryEntry;
  rendererMethods: RendererDomMethods;
  originals: RendererDomMethodOriginals;
  generation: TerminalGenerationIdentity;
  unsubscribeGeneration: (() => void) | null;
  retirementFrame: TerminalDomRetirementFrame | null;
};

const suppressTerminalDomMutation: RendererDomMethod = () => undefined;

export function createSmokeTerminalDomFaultMethods(): SmokeTerminalDomFaultMethods {
  const holdsBySessionId = new Map<string, TerminalDomHold>();

  const restoreTerminalDomHold = (hold: TerminalDomHold): void => {
    if (holdsBySessionId.get(hold.sessionId) !== hold) return;
    holdsBySessionId.delete(hold.sessionId);
    if (hold.retirementFrame !== null) {
      window.cancelAnimationFrame(hold.retirementFrame);
      hold.retirementFrame = null;
    }
    hold.unsubscribeGeneration?.();
    hold.unsubscribeGeneration = null;
    restoreRendererDomMethods(hold.rendererMethods, hold.originals);
  };

  const scheduleRetirementCheck = (hold: TerminalDomHold): void => {
    hold.retirementFrame = window.requestAnimationFrame(() => {
      hold.retirementFrame = null;
      if (holdsBySessionId.get(hold.sessionId) !== hold) return;
      if (rendererRegistryEntry(hold.sessionId) !== hold.rendererEntry) {
        restoreTerminalDomHold(hold);
        return;
      }
      scheduleRetirementCheck(hold);
    });
  };

  return {
    holdTerminalDomForCurrentGeneration(sessionId) {
      const generation = currentSyncV2TerminalState();
      if (!generation?.ready) {
        throw new Error(`terminal DOM hold requires a ready terminal generation: ${sessionId}`);
      }
      const activeHold = holdsBySessionId.get(sessionId);
      if (activeHold) {
        if (
          rendererRegistryEntry(sessionId) !== activeHold.rendererEntry
          || !sameTerminalGeneration(generation, activeHold.generation)
        ) {
          restoreTerminalDomHold(activeHold);
        } else {
          throw new Error(`terminal DOM hold already active for ${sessionId}`);
        }
      }
      const rendererEntry = rendererRegistryEntry(sessionId);
      if (!rendererEntry) {
        throw new Error(`terminal DOM hold requires a registered renderer: ${sessionId}`);
      }
      const rendererMethods = rendererEntry.renderer as unknown as RendererDomMethods;
      const originals = captureRendererDomMethods(rendererMethods, sessionId);
      const hold: TerminalDomHold = {
        sessionId,
        rendererEntry,
        rendererMethods,
        originals,
        generation: terminalGenerationIdentity(generation),
        unsubscribeGeneration: null,
        retirementFrame: null,
      };
      suppressRendererDomMethods(rendererMethods);
      holdsBySessionId.set(sessionId, hold);
      const unsubscribeGeneration = registerSyncV2GenerationHandler((nextGeneration) => {
        if (
          rendererRegistryEntry(sessionId) !== hold.rendererEntry
          || !sameTerminalGeneration(nextGeneration, hold.generation)
        ) restoreTerminalDomHold(hold);
      });
      if (holdsBySessionId.get(sessionId) !== hold) {
        unsubscribeGeneration();
        return;
      }
      hold.unsubscribeGeneration = unsubscribeGeneration;
      scheduleRetirementCheck(hold);
    },
    releaseTerminalDomHold(sessionId) {
      const hold = holdsBySessionId.get(sessionId);
      if (hold) restoreTerminalDomHold(hold);
    },
  };
}

function terminalGenerationIdentity(state: SyncV2TerminalState): TerminalGenerationIdentity {
  return {
    socketGeneration: state.socketGeneration,
    socketId: state.socketId,
    processEpoch: state.processEpoch,
    domainGeneration: state.domainGeneration,
  };
}

function sameTerminalGeneration(
  state: SyncV2TerminalState | null,
  generation: TerminalGenerationIdentity,
): boolean {
  return state !== null
    && state.socketGeneration === generation.socketGeneration
    && state.socketId === generation.socketId
    && state.processEpoch === generation.processEpoch
    && state.domainGeneration === generation.domainGeneration;
}

function captureRendererDomMethods(
  rendererMethods: RendererDomMethods,
  sessionId: string,
): RendererDomMethodOriginals {
  if (
    typeof rendererMethods.renderFull !== "function"
    || typeof rendererMethods.renderViewportRepair !== "function"
    || typeof rendererMethods.renderDelta !== "function"
    || typeof rendererMethods._markReconciledIfCurrent !== "function"
  ) {
    throw new Error(`terminal renderer DOM methods are unavailable: ${sessionId}`);
  }
  return {
    renderFull: rendererMethods.renderFull,
    renderFullWasOwn: Object.hasOwn(rendererMethods, "renderFull"),
    renderViewportRepair: rendererMethods.renderViewportRepair,
    renderViewportRepairWasOwn: Object.hasOwn(rendererMethods, "renderViewportRepair"),
    renderDelta: rendererMethods.renderDelta,
    renderDeltaWasOwn: Object.hasOwn(rendererMethods, "renderDelta"),
    markReconciledIfCurrent: rendererMethods._markReconciledIfCurrent,
    markReconciledIfCurrentWasOwn: Object.hasOwn(rendererMethods, "_markReconciledIfCurrent"),
  };
}

function suppressRendererDomMethods(rendererMethods: RendererDomMethods): void {
  rendererMethods.renderFull = suppressTerminalDomMutation;
  rendererMethods.renderViewportRepair = suppressTerminalDomMutation;
  rendererMethods.renderDelta = suppressTerminalDomMutation;
  rendererMethods._markReconciledIfCurrent = suppressTerminalDomMutation;
}

function restoreRendererDomMethods(
  rendererMethods: RendererDomMethods,
  originals: RendererDomMethodOriginals,
): void {
  restoreRendererDomMethod(
    rendererMethods,
    "renderFull",
    originals.renderFull,
    originals.renderFullWasOwn,
  );
  restoreRendererDomMethod(
    rendererMethods,
    "renderViewportRepair",
    originals.renderViewportRepair,
    originals.renderViewportRepairWasOwn,
  );
  restoreRendererDomMethod(
    rendererMethods,
    "renderDelta",
    originals.renderDelta,
    originals.renderDeltaWasOwn,
  );
  restoreRendererDomMethod(
    rendererMethods,
    "_markReconciledIfCurrent",
    originals.markReconciledIfCurrent,
    originals.markReconciledIfCurrentWasOwn,
  );
}

function restoreRendererDomMethod(
  rendererMethods: RendererDomMethods,
  methodName: keyof RendererDomMethods,
  original: RendererDomMethod,
  wasOwn: boolean,
): void {
  if (wasOwn) {
    rendererMethods[methodName] = original;
    return;
  }
  delete (rendererMethods as unknown as Record<string, unknown>)[methodName];
}
