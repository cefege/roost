// Live console for a cross-worker rsync transfer. Subscribes to coord's
// transfersOutput stream and renders it via the shared ConsoleModalShell,
// docked bottom-right so the rest of the UI stays usable during the transfer.

import { createSignal, onMount, onCleanup } from "solid-js";
import { coordClient } from "../connect.ts";
import { ConsoleModalShell, type ConsoleDone } from "./ConsoleModalShell.tsx";

interface Props {
  jobId: string;
  srcLabel: string;
  dstLabel: string;
  srcPath: string;
  dstPath: string;
  onClose: () => void;
}

export function TransferConsoleModal(props: Props) {
  const [lines, setLines] = createSignal<string[]>([]);
  const [done, setDone] = createSignal<ConsoleDone | null>(null);

  let cancelled = false;
  onMount(() => {
    (async () => {
      try {
        const stream = coordClient.transfersOutput({ jobId: props.jobId });
        for await (const f of stream) {
          if (cancelled) break;
          if (f.kind === "line") {
            setLines((prev) => [...prev, f.text]);
          } else {
            setDone({ exit: f.exit === -1 ? null : f.exit, error: f.error || undefined });
          }
        }
      } catch (err) {
        if (!cancelled) {
          console.error("[transfer-console] stream error", err);
          setDone({ exit: null, error: (err as Error).message });
        }
      }
    })();
    onCleanup(() => { cancelled = true; });
  });

  const headerText = () => {
    const d = done();
    if (!d) return `Transferring ${props.srcLabel}:${props.srcPath} → ${props.dstLabel}:${props.dstPath}`;
    if (d.exit === 0 && !d.error) return `Transferred ${props.srcLabel} → ${props.dstLabel} ✓`;
    return `Transfer failed: ${d.error ?? `rsync exit ${d.exit}`}`;
  };

  return (
    <ConsoleModalShell
      testId="transfer-console"
      width="min(560px, calc(100vw - 40px))"
      maxHeight="min(420px, calc(100vh - 40px))"
      waitingText="Waiting for rsync output…"
      runningHint="transfer continues"
      lines={lines}
      done={done}
      headerText={headerText}
      onClose={props.onClose}
    />
  );
}
