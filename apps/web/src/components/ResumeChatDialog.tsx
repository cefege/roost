// ResumeChatDialog — pick an existing omp conversation and continue it in the
// browser. Mount once in the app shell; toggle via resumeChatDialogStore.
//
// This is the deliberate, one-shot replacement for the mirror engine: instead
// of Roost attaching to a terminal's live omp, you pick that conversation's
// TRANSCRIPT and a fresh agent session resumes it. Nothing stays coupled.
//
// A transcript a live omp is still writing is listed but NOT selectable: two
// processes appending to one session file corrupt it, and the worker refuses
// the spawn anyway — saying so up front beats an error toast after the tap.
//
// Callers: App.tsx (mount), CommandPalette + sidebar ("Resume chat").

import type { Component } from "solid-js";
import { createResource, createSignal, For, Show } from "solid-js";
import { useNavigate } from "@solidjs/router";
import type { WorkerFp } from "@roost/shared/wire";
import { Dialog } from "./Settings/md/Dialog.tsx";
import { rootStore } from "../store/root.ts";
import { workerOnline } from "../store/sync.ts";
import { chatWorkerCandidates } from "../lib/quickChat.ts";
import { listOmpTranscripts, startResumedChat, type OmpTranscript } from "../lib/resumeChat.ts";
import { shortCwd } from "../lib/sidebarFormat.ts";

const [open, setOpen] = createSignal(false);

export const resumeChatDialogStore = {
  isOpen: open,
  open(): void { setOpen(true); },
  close(): void { setOpen(false); },
} as const;

/** "42s" / "3m" / "2h" / "4d" — enough to tell yesterday's thread from last
 *  month's without a date format nobody scans. */
function age(ms: number): string {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  return `${Math.round(s / 86400)}d`;
}

export const ResumeChatDialog: Component = () => {
  const navigate = useNavigate();
  // Default to the machine a new chat would land on, so the common case needs
  // no picking at all.
  const [workerFp, setWorkerFp] = createSignal<WorkerFp | null>(null);
  const target = (): WorkerFp | null => workerFp() ?? chatWorkerCandidates()[0] ?? null;

  // Keyed on (open, worker): the scan is a worker-side filesystem walk, so it
  // runs when the dialog is actually open and re-runs on a machine switch —
  // never on a background render.
  const [items] = createResource(
    () => (open() ? target() : null),
    (fp: WorkerFp) => listOmpTranscripts(fp),
  );

  const onlineWorkers = () => Object.values(rootStore.workers).filter(workerOnline);

  const pick = (t: OmpTranscript) => {
    const fp = target();
    if (!fp || t.active) return;
    resumeChatDialogStore.close();
    void startResumedChat(navigate, fp, t);
  };

  return (
    <Dialog open={open()} onClose={() => resumeChatDialogStore.close()} headline="Resume a chat">
      <Show when={open()}>
        <div class="resume-chat" data-testid="resume-chat-dialog">
          <Show when={onlineWorkers().length > 1}>
            <div class="resume-chat__machines">
              <For each={onlineWorkers()}>
                {(w) => (
                  <button
                    type="button"
                    class="resume-chat__machine"
                    aria-pressed={target() === w.fp}
                    onClick={() => setWorkerFp(w.fp)}
                  >{w.label || w.fp.slice(0, 8)}</button>
                )}
              </For>
            </div>
          </Show>

          <Show when={!items.loading} fallback={<div class="resume-chat__empty">Scanning…</div>}>
            <Show
              when={(items() ?? []).length > 0}
              fallback={<div class="resume-chat__empty">No omp conversations found on this machine.</div>}
            >
              <ul class="resume-chat__list">
                <For each={items()}>
                  {(t) => (
                    <li>
                      <button
                        type="button"
                        class="resume-chat__row"
                        data-testid="resume-chat-row"
                        data-active={t.active ? "true" : undefined}
                        disabled={t.active}
                        title={t.active
                          ? "An omp process still has this session open — quit it in the terminal first"
                          : t.path}
                        onClick={() => pick(t)}
                      >
                        <span class="resume-chat__title">{t.title || "(untitled)"}</span>
                        <span class="resume-chat__meta">
                          <Show when={t.active}><span class="resume-chat__live">live</span></Show>
                          <span>{shortCwd(t.cwd)}</span>
                          <span>{age(t.updatedAt)}</span>
                        </span>
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </Show>
          <Show when={items.error}>
            <div class="resume-chat__empty">{String(items.error)}</div>
          </Show>
        </div>
      </Show>
    </Dialog>
  );
};
