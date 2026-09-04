// WhatsNewDialog — surfaces /whatsnew.json on first boot of each app version.
// Compares import.meta.env.VITE_APP_VERSION against
// localStorage["roost.whatsNew.lastSeenVersion"]. When different, fetches
// /whatsnew.json and renders the dialog for the matching version entry.
// Callers: App.tsx (mounted inside the protected overlay shell; internal Show gate).
// Depends on: @roost/shared/log (warn on fetch failure).
//
// whatsnew.json shape: array of { version, date, title, items[] }.
// Dialog shows the entry whose version === current app version.

import { type Component, createSignal, onMount, For, Show } from "solid-js";
import { log } from "@roost/shared/log";
import { Dialog, Button } from "./Settings/md/primitives.tsx";

const STORAGE_KEY = "roost.whatsNew.lastSeenVersion";

// Matches the shape of each element in /whatsnew.json
interface WhatsNewEntry {
  version: string;
  date: string;
  title: string;
  items: string[];
}

function readCurrentVersion(): string {
  // Vite injects VITE_APP_VERSION via env; fall back to "0.0.0-dev".
  const v = (import.meta as unknown as { env: Record<string, string> }).env?.VITE_APP_VERSION;
  return typeof v === "string" && v.length > 0 ? v : "0.0.0-dev";
}

function readLastSeen(): string | null {
  try {
    return localStorage.getItem(STORAGE_KEY);
  } catch {
    return null;
  }
}

function writeLastSeen(version: string): void {
  try {
    localStorage.setItem(STORAGE_KEY, version);
  } catch {
    // localStorage disabled — accept re-show on next boot
  }
}

export const WhatsNewDialog: Component = () => {
  const [entry, setEntry] = createSignal<WhatsNewEntry | null>(null);
  const [open, setOpen] = createSignal(false);

  onMount(() => {
    const current = readCurrentVersion();
    const lastSeen = readLastSeen();
    if (lastSeen === current) return;

    void (async () => {
      try {
        const resp = await fetch("/whatsnew.json", { cache: "no-cache" });
        if (!resp.ok) return;
        const data = (await resp.json()) as WhatsNewEntry[];
        if (!Array.isArray(data)) return;
        // Find the entry matching the running version.
        const match = data.find((e) => e.version === current);
        if (!match) return;
        setEntry(match);
        setOpen(true);
      } catch (e) {
        log.warn("WhatsNewDialog", "fetch_failed", { err: String(e) });
      }
    })();
  });

  function dismiss() {
    setOpen(false);
    const e = entry();
    if (e) writeLastSeen(e.version);
  }

  return (
    <Dialog
      open={open() && entry() != null}
      onClose={dismiss}
      headline={entry()?.title}
      actions={
        <Button variant="filled" onClick={dismiss} data-testid="whats-new-dismiss">
          Got it
        </Button>
      }
    >
      <Show when={entry()}>
        {(e) => (
          <>
            <div style={{ "font-size": "var(--md-label-m-size, 12px)", color: "var(--text-lo)", "margin-bottom": "12px" }}>
              v{e().version} · {e().date}
            </div>
            <ul style={{ margin: "0", padding: "0 0 0 18px", display: "flex", "flex-direction": "column", gap: "6px" }}>
              <For each={e().items}>
                {(item) => (
                  <li style={{ "font-size": "var(--md-body-m-size, 14px)", color: "var(--text-hi)", "line-height": "1.5" }}>{item}</li>
                )}
              </For>
            </ul>
          </>
        )}
      </Show>
    </Dialog>
  );
};
