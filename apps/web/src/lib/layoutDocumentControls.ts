// Owns TerminalDeck's local layout copy, download, preview, and apply workflow.
// Dashboard, folder, and import-attempt identity fence every asynchronous edge;
// application revalidates current membership before one atomic store commit.
// The controller has no JSX and sends no coordinator traffic.

import { batch, createEffect, createSignal, onCleanup } from "solid-js";
import type { LayoutDocumentV1 } from "@roost/shared/layout-document";
import {
  applyLayoutDocument,
  exportLayoutDocument,
  validateLayoutDocumentImport,
} from "../store/paneLayoutDocument.ts";
import {
  downloadLayoutDocument,
  pickLayoutDocumentFile,
  serializeLayoutDocument,
} from "./layoutDocumentFile.ts";
import { copyToClipboard } from "./clipboard.ts";
import { addToast } from "../store/toastStore.ts";
import {
  captureDashboardResourceToken,
  isCurrentDashboardResourceToken,
} from "../store/dashboard-selection.ts";
import type { DashboardResourceToken } from "../store/dashboard-selection.ts";
import { liveSessionIdsForFolder } from "../store/selectors.ts";
import { clearSpotlight } from "../store/spotlight.ts";
import { sessionHref } from "../routes.ts";

interface LayoutImportAttempt {
  folderKey: string;
  dashboardToken: DashboardResourceToken;
}

interface LayoutImportPreviewState {
  attempt: LayoutImportAttempt;
  fileName: string;
  document: LayoutDocumentV1 | null;
  error: string | null;
  reading: boolean;
}

export interface LayoutDocumentControlContext {
  folderKey: () => string | null;
  navigate: (href: string) => void;
}

export function createLayoutDocumentControls(context: LayoutDocumentControlContext) {
  const [preview, setPreview] = createSignal<LayoutImportPreviewState | null>(null);
  const [activeImport, setActiveImport] = createSignal<LayoutImportAttempt | null>(null);
  let disposed = false;

  function isActiveImport(attempt: LayoutImportAttempt): boolean {
    return !disposed
      && activeImport() === attempt
      && isCurrentDashboardResourceToken(attempt.dashboardToken)
      && context.folderKey() === attempt.folderKey;
  }

  function cancelImport(expected?: LayoutImportAttempt): void {
    if (expected && activeImport() !== expected) return;
    setActiveImport(null);
    setPreview((current) =>
      expected && current?.attempt !== expected ? current : null);
  }

  createEffect(() => {
    const attempt = activeImport();
    if (attempt && !isActiveImport(attempt)) cancelImport(attempt);
  });
  onCleanup(() => {
    disposed = true;
    setActiveImport(null);
    setPreview(null);
  });

  function currentDocument(): LayoutDocumentV1 {
    const folderKey = context.folderKey();
    if (!folderKey) throw new Error("No active folder has a layout to export.");
    return exportLayoutDocument(folderKey, liveSessionIdsForFolder(folderKey));
  }

  function copyLayout(): void {
    const dashboardToken = captureDashboardResourceToken();
    const folderKey = context.folderKey();
    let serialized: string;
    try {
      serialized = serializeLayoutDocument(currentDocument());
    } catch (error) {
      addToast(layoutDocumentErrorMessage(error), "err");
      return;
    }
    void copyToClipboard(serialized).then((copied) => {
      if (
        disposed
        || !isCurrentDashboardResourceToken(dashboardToken)
        || context.folderKey() !== folderKey
      ) return;
      addToast(copied ? "Layout copied." : "Clipboard access was denied.", copied ? "ok" : "err");
    });
  }

  function downloadLayout(): void {
    try {
      downloadLayoutDocument(currentDocument());
      addToast("Layout downloaded.", "ok");
    } catch (error) {
      addToast(layoutDocumentErrorMessage(error), "err");
    }
  }

  async function readImport(file: File, attempt: LayoutImportAttempt): Promise<void> {
    try {
      const source = await file.text();
      if (!isActiveImport(attempt)) return;
      const candidate: unknown = JSON.parse(source);
      const document = validateLayoutDocumentImport(
        candidate,
        liveSessionIdsForFolder(attempt.folderKey),
      );
      if (!isActiveImport(attempt)) return;
      setPreview({
        attempt,
        fileName: file.name,
        document,
        error: null,
        reading: false,
      });
    } catch (error) {
      if (!isActiveImport(attempt)) return;
      setPreview({
        attempt,
        fileName: file.name,
        document: null,
        error: layoutDocumentErrorMessage(error),
        reading: false,
      });
    }
  }

  function importLayout(): void {
    const folderKey = context.folderKey();
    if (!folderKey) return;
    const attempt: LayoutImportAttempt = {
      folderKey,
      dashboardToken: captureDashboardResourceToken(),
    };
    setActiveImport(attempt);
    setPreview(null);
    pickLayoutDocumentFile((file) => {
      if (!isActiveImport(attempt)) return;
      setPreview({
        attempt,
        fileName: file.name,
        document: null,
        error: null,
        reading: true,
      });
      void readImport(file, attempt);
    });
  }

  function applyImportedLayout(): void {
    const pending = preview();
    if (!pending?.document) return;
    if (!isActiveImport(pending.attempt)) {
      cancelImport(pending.attempt);
      return;
    }
    try {
      batch(() => {
        const applied = applyLayoutDocument(
          pending.attempt.folderKey,
          pending.document,
          liveSessionIdsForFolder(pending.attempt.folderKey),
        );
        clearSpotlight();
        if (applied.selectedSessionId) {
          context.navigate(sessionHref(applied.selectedSessionId));
        }
      });
      cancelImport(pending.attempt);
      addToast("Layout applied.", "ok");
    } catch (error) {
      if (!isActiveImport(pending.attempt)) return;
      setPreview((current) => current === pending
        ? { ...current, error: layoutDocumentErrorMessage(error) }
        : current);
    }
  }

  return {
    applyImportedLayout,
    closeImport: () => cancelImport(),
    copyLayout,
    downloadLayout,
    importLayout,
    preview,
  };
}

function layoutDocumentErrorMessage(error: unknown): string {
  if (error instanceof SyntaxError) return "The selected file is not valid JSON.";
  return error instanceof Error ? error.message : "The selected layout is invalid.";
}
