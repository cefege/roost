// New-folder dialog for the browse page: name field + Create/Cancel. Split out
// of BrowsePage.tsx; the page owns the open/name/busy signals, the mkdir RPC,
// and the post-create navigation, and hands the input element back to the page
// so its open path can focus it.
//
// Callers: BrowsePage.tsx (WorkerBrowsePage).

import { Dialog, Button, TextField } from "./Settings/md/primitives.tsx";

export function NewFolderDialog(props: {
  open: boolean;
  name: string;
  busy: boolean;
  /** Resolved directory the folder lands in (cwdNow) — shown as the hint. */
  targetPath: string;
  onName: (v: string) => void;
  onClose: () => void;
  onCreate: () => void;
  setInputRef: (el: HTMLElement) => void;
}) {
  return (
    <Dialog
      open={props.open}
      onClose={props.onClose}
      headline="New folder"
      actions={
        <>
          <span style={{ flex: "1" }} />
          <Button variant="text" onClick={props.onClose}>Cancel</Button>
          <Button variant="filled" data-testid="newfolder-confirm"
            onClick={props.onCreate} disabled={props.busy || !props.name.trim()}>
            {props.busy ? "Creating…" : "Create"}
          </Button>
        </>
      }
    >
      <div style={{ display: "flex", "flex-direction": "column", gap: "12px", "min-width": "320px" }}>
        <TextField value={props.name} onInput={(v) => props.onName(v)} label="Folder name"
          testId="newfolder-input" style={{ width: "100%" }} ref={props.setInputRef}
          onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); props.onCreate(); } }} />
        <div style={{ "font-size": "12px", color: "var(--md-sys-color-on-surface-variant)" }}>
          Creates a folder in {props.targetPath}.
        </div>
      </div>
    </Dialog>
  );
}
