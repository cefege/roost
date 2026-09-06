// One-shot handoff from dashboard content results to pane-local terminal find.
// A mounted pane consumes immediately; a cold pane consumes on registration.
// Dashboard cutover clears both callbacks and pending session identities.

import type { TerminalFind } from "./terminalFindController.ts";
import type { TerminalFindPreferredMatch } from "./terminalFindHandoff.ts";

export interface TerminalFindIntentOptions {
  readonly caseSensitive?: boolean;
  readonly preferredGlobalMatch?: TerminalFindPreferredMatch;
}

interface TerminalFindIntent {
  readonly literalQuery: string;
  readonly caseSensitive: boolean;
  readonly preferredGlobalMatch?: TerminalFindPreferredMatch;
}

interface TerminalFindRegistration {
  readonly find: Pick<TerminalFind, "openFind" | "setQuery">;
}

class TerminalFindIntentRegistry {
  readonly #registrations = new Map<string, TerminalFindRegistration>();
  readonly #pending = new Map<string, TerminalFindIntent>();

  request(
    sessionId: string,
    literalQuery: string,
    options: TerminalFindIntentOptions,
  ): void {
    const intent: TerminalFindIntent = {
      literalQuery,
      caseSensitive: options.caseSensitive ?? false,
      preferredGlobalMatch: options.preferredGlobalMatch,
    };
    const registration = this.#registrations.get(sessionId);
    if (registration) {
      this.#apply(registration, intent);
      return;
    }
    this.#pending.set(sessionId, intent);
  }

  register(
    sessionId: string,
    find: Pick<TerminalFind, "openFind" | "setQuery">,
  ): () => void {
    const registration = { find };
    this.#registrations.set(sessionId, registration);
    const pending = this.#pending.get(sessionId);
    if (pending) {
      this.#pending.delete(sessionId);
      this.#apply(registration, pending);
    }
    return () => {
      if (this.#registrations.get(sessionId) === registration) {
        this.#registrations.delete(sessionId);
      }
    };
  }

  reset(): void {
    this.#registrations.clear();
    this.#pending.clear();
  }

  #apply(registration: TerminalFindRegistration, intent: TerminalFindIntent): void {
    registration.find.openFind();
    registration.find.setQuery(intent.literalQuery, {
      literal: true,
      caseSensitive: intent.caseSensitive,
      preferredMatch: intent.preferredGlobalMatch,
    });
  }
}

const terminalFindIntentRegistry = new TerminalFindIntentRegistry();

export function requestTerminalFind(
  sessionId: string,
  literalQuery: string,
  options: TerminalFindIntentOptions = {},
): void {
  terminalFindIntentRegistry.request(sessionId, literalQuery, options);
}

export function registerTerminalFind(
  sessionId: string,
  find: Pick<TerminalFind, "openFind" | "setQuery">,
): () => void {
  return terminalFindIntentRegistry.register(sessionId, find);
}

export function resetTerminalFindIntentsForDashboardSwitch(): void {
  terminalFindIntentRegistry.reset();
}

export function _resetTerminalFindIntentsForTest(): void {
  terminalFindIntentRegistry.reset();
}
