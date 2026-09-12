// Rename-dialog host for a session context-menu action.
// Shared Dialog owns focus containment, dismissal, and modal presentation.
// The store supplies one active rename request and routes mutation outcomes.

import { createSignal, createEffect, Show } from "solid-js";
import { coordClient } from "../connect.ts";
import { Dialog, Button, TextField } from "./Settings/md/primitives.tsx";
import { activeRenameDialog, closeRenameDialog } from "../store/renameDialog.ts";
import { addToast } from "../store/toastStore.ts";

export function RenameDialogHost() {
  const [name, setName] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  let inputEl: HTMLElement | undefined;

  // Re-seed input + focus each time a new rename target opens.
  createEffect(() => {
    const ctx = activeRenameDialog();
    if (ctx) {
      setName(ctx.currentTitle);
      setBusy(false);
      queueMicrotask(() => inputEl?.focus());
    }
  });

  // title="" clears the override (revert to auto). Coord caps + folds.
  // Two commit paths: a generic onCommit (folder → workspace rename) when
  // supplied, else the session-rename RPC keyed on sessionId.
  async function commit(title: string) {
    const ctx = activeRenameDialog();
    if (!ctx || busy()) return;
    setBusy(true);
    try {
      if (ctx.onCommit) {
        await ctx.onCommit(title.trim());
      } else if (ctx.sessionId) {
        const res = await coordClient.sessionsRename({ sessionId: ctx.sessionId, title: title.trim() });
        if (!res.ok) addToast("Rename failed: session not found", "err");
      }
      closeRenameDialog();
    } catch (err) {
      addToast(`Rename failed: ${(err as Error).message}`, "err");
      setBusy(false);
    }
  }

  return (
    <Dialog
      open={!!activeRenameDialog()}
      onClose={closeRenameDialog}
      headline={activeRenameDialog()?.headline ?? "Rename terminal"}
      actions={
        <>
          <Show when={activeRenameDialog()?.hasCustom}>
            <Button variant="ghost" data-testid="rename-reset" onClick={() => void commit("")}>
              Reset to auto
            </Button>
          </Show>
          <span style={{ flex: "1" }} />
          <Button variant="outline" onClick={() => closeRenameDialog()}>Cancel</Button>
          <Button variant="default" data-testid="rename-confirm" onClick={() => void commit(name())} disabled={busy()}>{busy() ? "Renaming…" : "Rename"}</Button>
        </>
      }
    >
      <div style={{ display: "flex", "flex-direction": "column", gap: "12px", "min-width": "320px" }}>
        <TextField
          value={name()}
          onInput={(v) => setName(v)}
          label="Custom name"
          testId="rename-input"
          style={{ width: "100%" }}
          ref={(el) => { inputEl = el; }}
          onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void commit(name()); } }}
        />
        <div style={{ "font-size": "12px", color: "var(--md-sys-color-on-surface-variant)" }}>
          <Show
            when={activeRenameDialog()?.onCommit}
            fallback="Overrides the auto title. Stays until you change it — agent/terminal title updates won't touch it. Clear it to go back to auto."
          >
            Names this folder as a workspace. Shows on every device you're signed in on.
          </Show>
        </div>
      </div>
    </Dialog>
  );
}
