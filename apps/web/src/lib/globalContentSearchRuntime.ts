// Dashboard-bound lifecycle owner for global-content controllers and terminal-find intents.
// Controllers register a reset callback; dashboard selection invokes this owner
// before old sessions and transport state are released.

import { resetTerminalFindIntentsForDashboardSwitch } from "./terminalFindIntent.ts";

export interface DashboardBoundContentSearch {
  resetForDashboardCutover(): void;
  resumeAfterDashboardCutover(): void;
}

export class _GlobalContentSearchRuntime {
  readonly #controllers = new Set<DashboardBoundContentSearch>();
  #suspended = false;

  register(controller: DashboardBoundContentSearch): () => void {
    this.#controllers.add(controller);
    if (this.#suspended) controller.resetForDashboardCutover();
    return () => this.#controllers.delete(controller);
  }

  suspend(): void {
    this.#suspended = true;
    for (const controller of [...this.#controllers]) {
      controller.resetForDashboardCutover();
    }
  }

  resume(): void {
    if (!this.#suspended) return;
    this.#suspended = false;
    for (const controller of [...this.#controllers]) {
      controller.resumeAfterDashboardCutover();
    }
  }
}

const globalContentSearchRuntime = new _GlobalContentSearchRuntime();

export function registerDashboardBoundContentSearch(
  controller: DashboardBoundContentSearch,
): () => void {
  return globalContentSearchRuntime.register(controller);
}


export function resetDashboardSearchRuntime(): void {
  resetTerminalFindIntentsForDashboardSwitch();
  globalContentSearchRuntime.suspend();
}

export function resumeDashboardSearchRuntime(): void {
  globalContentSearchRuntime.resume();
}
