// New-folder flow for the folder picker: dialog state, name validation before
// any RPC, the auth-fenced FilesMkdir, and the reader-facing failure the dialog
// shows in place of a toast. WorkerBrowsePage supplies the target directory and
// the sibling names, and navigates into whatever the worker resolved.
//
// Callers: WorkerBrowsePage.tsx.

import { createSignal, type Accessor } from "solid-js";
import { diag } from "@roost/observability/diag";
import { coordClient } from "../connect.ts";
import {
  captureAuthResourceToken,
  isCurrentAuthResourceToken,
} from "../store/auth-boundary.ts";
import { browseErrorMessage } from "../lib/browseErrorMessage.ts";
import { validateNewFolderName } from "../lib/folderNameValidation.ts";
import { childPath } from "../lib/folderPalette.ts";
import type { WorkerFp } from "@roost/protocol/wire";

export interface BrowseNewFolder {
  open: Accessor<boolean>;
  name: Accessor<string>;
  busy: Accessor<boolean>;
  error: Accessor<string | null>;
  /** Clears the previous attempt, opens the dialog, focuses the field. */
  begin: () => void;
  setName: (value: string) => void;
  close: () => void;
  commit: () => void;
  setInputRef: (element: HTMLElement) => void;
}

export function createBrowseNewFolder(deps: {
  workerFp: Accessor<string>;
  /** Directory the folder lands in. */
  parent: Accessor<string>;
  /** Existing directory names here, hidden ones included. */
  siblings: Accessor<readonly string[]>;
  scoped: Accessor<boolean>;
  onCreated: (resolvedPath: string) => void;
}): BrowseNewFolder {
  const [open, setOpen] = createSignal(false);
  const [name, setName] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  let input: HTMLElement | undefined;

  function close(): void {
    setOpen(false);
    setBusy(false);
    setError(null);
  }

  async function run(): Promise<void> {
    if (busy()) return;
    const validation = validateNewFolderName(name(), deps.siblings());
    if (!validation.ok) {
      setError(validation.message);
      return;
    }
    const fp = deps.workerFp();
    if (!fp || !deps.scoped()) {
      setError("This machine isn't available right now.");
      return;
    }
    const target = childPath(fp, deps.parent(), name().trim());
    const authToken = captureAuthResourceToken();
    setError(null);
    setBusy(true);
    diag("browse.mkdir", { worker_fp: fp, path: target });
    try {
      const response = await coordClient.filesMkdir({
        workerFp: fp as unknown as WorkerFp,
        path: target,
      });
      if (!isCurrentAuthResourceToken(authToken)) return;
      setOpen(false);
      deps.onCreated(response.resolvedPath || target);
    } catch (failure) {
      if (!isCurrentAuthResourceToken(authToken)) return;
      setError(browseErrorMessage(failure));
      // The mapped copy is what the user reads; the machine's own words stay in
      // the diag line so the failure remains greppable.
      diag("browse.mkdir_failed", {
        worker_fp: fp,
        path: target,
        error: failure instanceof Error ? failure.message : String(failure),
      });
    } finally {
      setBusy(false);
    }
  }

  return {
    open,
    name,
    busy,
    error,
    begin: () => {
      if (!deps.scoped()) return;
      setName("");
      setBusy(false);
      setError(null);
      setOpen(true);
      queueMicrotask(() => input?.focus());
    },
    setName: (value: string) => {
      setName(value);
      setError(null);
    },
    close,
    commit: () => { void run(); },
    setInputRef: (element: HTMLElement) => { input = element; },
  };
}
