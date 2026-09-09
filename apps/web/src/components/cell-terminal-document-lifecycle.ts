// Coordinates the page-lifecycle edges shared by every terminal in one document.
// CellTerminal lifecycles register their local reader and view transition callback here.
// Each document owns one listener set; per-pane geometry and rendering stay local.
// Delivery is synchronous so browser lifecycle-event ordering remains intact.

import { isPageVisible } from "../lib/pageVisible.ts";

export type CellTerminalDocumentLifecycleEvent =
  | "hidden"
  | "pagehide"
  | "visible"
  | "pageshow"
  | "resume";

export type CellTerminalDocumentLifecycleCallback =
  (event: CellTerminalDocumentLifecycleEvent) => void;

interface TerminalDocumentLifecycleRegistration {
  readonly callback: CellTerminalDocumentLifecycleCallback;
}

interface TerminalDocumentLifecycleOwner {
  readonly document: Document;
  readonly registrations: Set<TerminalDocumentLifecycleRegistration>;
  readonly window: Window;
  onPageHide(): void;
  onPageShow(): void;
  onResume(): void;
  onVisibilityChange(): void;
}

const terminalDocumentLifecycleOwners = new WeakMap<
  Document,
  TerminalDocumentLifecycleOwner
>();

/** Register one mounted terminal lifecycle with its document's shared page listeners. */
export function registerCellTerminalDocumentLifecycle(
  callback: CellTerminalDocumentLifecycleCallback,
): () => void {
  if (typeof document === "undefined" || typeof window === "undefined") {
    return () => undefined;
  }
  const owner = terminalDocumentLifecycleOwners.get(document)
    ?? createTerminalDocumentLifecycleOwner(document, window);
  const registration = { callback };
  owner.registrations.add(registration);
  let disposed = false;
  return () => {
    if (disposed) return;
    disposed = true;
    owner.registrations.delete(registration);
    if (owner.registrations.size !== 0) return;
    disposeTerminalDocumentLifecycleOwner(owner);
  };
}

function createTerminalDocumentLifecycleOwner(
  ownerDocument: Document,
  ownerWindow: Window,
): TerminalDocumentLifecycleOwner {
  const registrations = new Set<TerminalDocumentLifecycleRegistration>();
  const notify = (event: CellTerminalDocumentLifecycleEvent): void => {
    for (const registration of [...registrations]) {
      if (registrations.has(registration)) registration.callback(event);
    }
  };
  const owner: TerminalDocumentLifecycleOwner = {
    document: ownerDocument,
    registrations,
    window: ownerWindow,
    onVisibilityChange: () => notify(isPageVisible() ? "visible" : "hidden"),
    onPageHide: () => notify("pagehide"),
    onPageShow: () => notify(isPageVisible() ? "pageshow" : "hidden"),
    onResume: () => notify(isPageVisible() ? "resume" : "hidden"),
  };
  ownerDocument.addEventListener("visibilitychange", owner.onVisibilityChange);
  ownerDocument.addEventListener("resume", owner.onResume);
  ownerWindow.addEventListener("pagehide", owner.onPageHide);
  ownerWindow.addEventListener("pageshow", owner.onPageShow);
  terminalDocumentLifecycleOwners.set(ownerDocument, owner);
  return owner;
}

function disposeTerminalDocumentLifecycleOwner(
  owner: TerminalDocumentLifecycleOwner): void {
  owner.document.removeEventListener("visibilitychange", owner.onVisibilityChange);
  owner.document.removeEventListener("resume", owner.onResume);
  owner.window.removeEventListener("pagehide", owner.onPageHide);
  owner.window.removeEventListener("pageshow", owner.onPageShow);
  terminalDocumentLifecycleOwners.delete(owner.document);
}
