// New-folder prompt for the folder picker: name field, the inline failure the
// attempt produced, and Create/Cancel. Compact gets the app's standard
// bottom sheet, desktop the centered dialog. The page owns the open/name/busy/
// error signals, the mkdir RPC, and the post-create navigation, and hands the
// input element back so its open path can focus it.
//
// Callers: WorkerBrowsePage.tsx.

import { isCompact } from "../../browser/windowSizeClass.ts";
import { Dialog, Button, TextField } from "../Settings/md/primitives.tsx";

export function NewFolderDialog(props: {
  open: boolean;
  name: string;
  busy: boolean;
  /** Validation or RPC failure for this attempt — shown under the field. */
  error: string | null;
  /** Resolved directory the folder lands in (cwdNow) — shown as the hint. */
  targetPath: string;
  onName: (value: string) => void;
  onClose: () => void;
  onCreate: () => void;
  setInputRef: (element: HTMLElement) => void;
}) {
  return (
    <Dialog
      open={props.open}
      onClose={props.onClose}
      headline="New folder"
      class={isCompact() ? "roost-sheet--bottom" : undefined}
      actions={
        <>
          <Button variant="outline" onClick={props.onClose}>Cancel</Button>
          <Button variant="default" data-testid="newfolder-confirm"
            onClick={props.onCreate} disabled={props.busy || !props.name.trim()}>{props.busy ? "Creating…" : "Create"}</Button>
        </>
      }
    >
      <TextField
        value={props.name}
        onInput={props.onName}
        label="Folder name"
        testId="newfolder-input"
        ref={props.setInputRef}
        description={`Creates a folder in ${props.targetPath}.`}
        error={props.error ? <span role="alert">{props.error}</span> : null}
        onKeyDown={(event) => {
          if (event.key !== "Enter") return;
          event.preventDefault();
          props.onCreate();
        }}
      />
    </Dialog>
  );
}
