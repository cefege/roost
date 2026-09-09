// Credential-boundary lifecycle owner for global-content controllers and terminal-find intents.
// Controllers register a reset callback; the auth-boundary teardown invokes this owner
// before old sessions and transport state are released.

import { resetTerminalFindIntentsForAuthBoundary } from "./terminalFindIntent.ts";

export interface AuthBoundContentSearch {
  resetForAuthBoundary(): void;
  resumeAfterAuthBoundary(): void;
}

export class _GlobalContentSearchRuntime {
  readonly #controllers = new Set<AuthBoundContentSearch>();
  #suspended = false;

  register(controller: AuthBoundContentSearch): () => void {
    this.#controllers.add(controller);
    if (this.#suspended) controller.resetForAuthBoundary();
    return () => this.#controllers.delete(controller);
  }

  suspend(): void {
    this.#suspended = true;
    for (const controller of [...this.#controllers]) {
      controller.resetForAuthBoundary();
    }
  }

  resume(): void {
    if (!this.#suspended) return;
    this.#suspended = false;
    for (const controller of [...this.#controllers]) {
      controller.resumeAfterAuthBoundary();
    }
  }
}

const globalContentSearchRuntime = new _GlobalContentSearchRuntime();

export function registerAuthBoundContentSearch(
  controller: AuthBoundContentSearch,
): () => void {
  return globalContentSearchRuntime.register(controller);
}


export function resetContentSearchRuntimeForAuthBoundary(): void {
  resetTerminalFindIntentsForAuthBoundary();
  globalContentSearchRuntime.suspend();
}

export function resumeContentSearchRuntimeAfterAuthBoundary(): void {
  globalContentSearchRuntime.resume();
}
