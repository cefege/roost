// Cross-worker rsync transfer config dialog. Opened from SessionRow's
// right-click "Transfer files to…" menu. The source context (worker
// fp + cwd) is pre-filled from the row that triggered it; the user
// picks the destination worker + paths + options + clicks Start.
//
// On Start: coordClient.transfersStart fires, returns a jobId, dialog closes
// and openTransferConsole hands off to TransferConsoleHost which mounts
// the streaming output panel.

import { createSignal, createMemo, Show, onMount, onCleanup } from "solid-js";
import { Portal } from "solid-js/web";
import { coordClient } from "../connect.ts";
import { Select, TextField, Button, Checkbox } from "./Settings/md/primitives.tsx";
import { rootStore } from "../store/root.ts";
import { activeTransferDialog, closeTransferDialog } from "../lib/transferDialog.ts";
import { openTransferConsole } from "../store/transferConsole.ts";
import { addToast } from "../store/toastStore.ts";
import { animateOverlayPanel } from "../lib/overlayMotion.ts";

export function TransferDialogHost() {
  return (
    <Show when={activeTransferDialog()}>
      {(ctx) => <TransferDialog ctx={ctx()} />}
    </Show>
  );
}

interface DialogProps {
  ctx: { srcFp: string; srcLabel: string; srcPath: string };
}

function TransferDialog(props: DialogProps) {
  // Destination defaults: first OTHER worker, same path as src.
  const otherWorkers = createMemo(() =>
    Object.values(rootStore.workers).filter((w) => w.fp !== props.ctx.srcFp),
  );
  const [dstFp, setDstFp] = createSignal<string>(otherWorkers()[0]?.fp ?? "");
  const [dstPath, setDstPath] = createSignal(props.ctx.srcPath);
  const [deleteExtra, setDeleteExtra] = createSignal(false);
  const [dryRun, setDryRun] = createSignal(true);
  const [busy, setBusy] = createSignal(false);

  const dstLabel = createMemo(() => {
    const fp = dstFp();
    return rootStore.workers[fp]?.label ?? "(select)";
  });

  function onPointerDown(e: PointerEvent) {
    const target = e.target as Element | null;
    if (!target) return;
    if (target.closest("[data-testid=\"transfer-dialog\"]")) return;
    closeTransferDialog();
  }
  onMount(() => document.addEventListener("pointerdown", onPointerDown, true));
  onCleanup(() => document.removeEventListener("pointerdown", onPointerDown, true));

  async function onStart() {
    if (!dstFp() || !dstPath().trim()) {
      addToast("Pick a destination worker and path first.", "warn");
      return;
    }
    setBusy(true);
    try {
      const res = await coordClient.transfersStart({
        srcFp: props.ctx.srcFp,
        srcPath: props.ctx.srcPath,
        dstFp: dstFp(),
        dstPath: dstPath().trim(),
        deleteExtra: deleteExtra(),
        dryRun: dryRun(),
      });
      if (res.ok && res.jobId) {
        openTransferConsole({
          jobId: res.jobId,
          srcLabel: props.ctx.srcLabel,
          dstLabel: dstLabel(),
          srcPath: props.ctx.srcPath,
          dstPath: dstPath().trim(),
        });
        closeTransferDialog();
      } else {
        addToast(`Transfer failed: ${res.error ?? "unknown error"}`, "err");
      }
    } catch (err) {
      addToast(`Transfer failed: ${(err as Error).message}`, "err");
    } finally {
      setBusy(false);
    }
  }

  return (
    <Portal mount={document.body}>
      <div
        style={{
          position: "fixed",
          inset: "0",
          background: "color-mix(in srgb, var(--md-scrim) 45%, transparent)",
          "z-index": "9997",
          display: "flex",
          "align-items": "center",
          "justify-content": "center",
          padding: "24px",
        }}
        onClick={(e) => { if (e.target === e.currentTarget) closeTransferDialog(); }}
      >
        <div
          ref={animateOverlayPanel}
          data-testid="transfer-dialog"
          style={{
            background: "var(--bg-elev-2)",
            border: "1px solid var(--border-strong)",
            "border-radius": "var(--md-shape-md)",
            "max-width": "520px",
            width: "100%",
            padding: "18px",
            display: "flex",
            "flex-direction": "column",
            gap: "14px",
            "box-shadow": "var(--md-elev-5)",
            color: "var(--md-on-surface-variant)",
            "font-size": "13px",
          }}
        >
          <div style={{ "font-weight": "600", "font-size": "var(--md-body-m-size)", color: "var(--md-on-surface)" }}>
            Transfer files
          </div>
          <FieldRow label="From">
            <span style={{ color: "var(--md-on-surface-dim)" }}>
              <strong>{props.ctx.srcLabel}</strong> · <code>{props.ctx.srcPath}</code>
            </span>
          </FieldRow>
          <FieldRow label="To worker">
            <Select
              value={dstFp()}
              onChange={(v) => setDstFp(v)}
              options={otherWorkers().map((w) => ({ value: w.fp, label: w.label }))}
              class="transfer-dialog-field"
            />
          </FieldRow>
          <FieldRow label="To path">
            <TextField
              value={dstPath()}
              onInput={(v) => setDstPath(v)}
              placeholder="/Users/you/Documents/…"
              style={{ width: "100%" }}
            />
          </FieldRow>
          <FieldRow label="Options">
            <span style={{ display: "inline-flex", gap: "12px", "flex-wrap": "wrap" }}>
              <CheckboxRow checked={dryRun()} onChange={setDryRun} label="Dry run (preview only)" />
              <CheckboxRow checked={deleteExtra()} onChange={setDeleteExtra} label="--delete (mirror, removes extras on destination)" />
            </span>
          </FieldRow>
          <div style={{ "font-size": "11px", color: "var(--md-on-surface-dim)" }}>
            Source worker SSHes to destination over tailnet. Both workers must be online.
          </div>
          <div style={{ display: "flex", "justify-content": "flex-end", gap: "8px", "margin-top": "4px" }}>
            <Button variant="text" onClick={() => closeTransferDialog()}>Cancel</Button>
            <Button
              variant="filled"
              onClick={onStart}
              disabled={busy() || !dstFp() || !dstPath().trim()}
            >{busy() ? "Starting…" : (dryRun() ? "Preview" : "Start transfer")}</Button>
          </div>
        </div>
      </div>
    </Portal>
  );
}

function FieldRow(props: { label: string; children: any }) {
  return (
    <label style={{ display: "flex", "flex-direction": "column", gap: "4px" }}>
      <span style={{ "font-size": "11px", color: "var(--md-on-surface-dim)", "text-transform": "uppercase", "letter-spacing": "0.04em" }}>
        {props.label}
      </span>
      {props.children}
    </label>
  );
}

// Checkbox primitive renders no visible text — pair it with a clickable label.
function CheckboxRow(props: { checked: boolean; onChange: (v: boolean) => void; label: string }) {
  return (
    <label style={{ display: "inline-flex", "align-items": "center", gap: "6px", cursor: "pointer" }}>
      <Checkbox checked={props.checked} onChange={props.onChange} label={props.label} />
      <span style={{ "font-size": "var(--md-body-s-size)" }} onClick={() => props.onChange(!props.checked)}>{props.label}</span>
    </label>
  );
}

