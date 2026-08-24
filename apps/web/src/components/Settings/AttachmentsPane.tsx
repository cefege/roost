// AttachmentsPane — files in ~/.roost/attachments/<sid>/ for the active
// session. Inject path into PTY, copy abs_path to clipboard, or delete.
// M3: filter Card (session dropdown + short-path toggle) on top; file
// list inside a second Card with mime-icon by extension and inline
// actions; expiry chip color reflects TTL state.
// Callers: SettingsRoot.tsx. Depends on: coordClient, rootStore,
// Sync v2 terminal outbound, attachments lib, md/primitives.

import { createResource, createSignal, For, Show, onCleanup } from "solid-js";
import { copyToClipboard } from "../../lib/clipboard.ts";
import { createTrackedTimeouts } from "../trackedTimeout.ts";
import { coordClient } from "../../connect.ts";
import { rootStore } from "../../store/root.ts";
import { sendUserTerminalInput } from "../../lib/userTerminalInput.ts";
import { getShortPathPref, setShortPathPref } from "../../lib/attachments.ts";
import { Card, Button, EmptyState, List, ListRow, Icon, Switch, Select } from "./md/primitives.tsx";
import { formatBytes } from "../../lib/format.ts";
import { isPageVisible } from "../../lib/pageVisible.ts";
import { supportedWorkerPlatform, workerPathBasename } from "../../lib/nativePath.ts";

function formatAge(mtimeMs: number): string {
  const ageMs = Date.now() - mtimeMs;
  const m = Math.round(ageMs / 60_000);
  if (m < 1) return "just now";
  if (m < 60) return `${m} min ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.round(h / 24)}d ago`;
}

const TTL_MS = 24 * 60 * 60 * 1000;
const WARN_THRESHOLD_MS = 60 * 60 * 1000;
function expiryStatus(mtimeMs: number): { state: "ok" | "warn" | "expired"; label: string } {
  const remaining = (mtimeMs + TTL_MS) - Date.now();
  if (remaining <= 0) return { state: "expired", label: "Expired" };
  if (remaining < WARN_THRESHOLD_MS) {
    const m = Math.max(1, Math.round(remaining / 60_000));
    return { state: "warn", label: `${m} min left` };
  }
  const h = Math.round(remaining / 3600_000);
  return { state: "ok", label: `${h}h left` };
}

function iconForFile(filename: string): string {
  const ext = filename.toLowerCase().split(".").pop() ?? "";
  if (["png", "jpg", "jpeg", "gif", "webp", "avif", "svg", "heic"].includes(ext)) return "image";
  if (["mp4", "mov", "mkv", "webm"].includes(ext)) return "movie";
  if (["mp3", "wav", "ogg", "flac", "m4a"].includes(ext)) return "audio_file";
  if (["pdf"].includes(ext)) return "picture_as_pdf";
  if (["zip", "tar", "gz", "tgz", "rar", "7z"].includes(ext)) return "folder_zip";
  if (["md", "txt", "log"].includes(ext)) return "description";
  if (["json", "yaml", "yml", "toml", "csv"].includes(ext)) return "data_object";
  if (["ts", "tsx", "js", "jsx", "rs", "py", "go", "c", "cpp", "h", "html", "css", "swift", "rb"].includes(ext)) return "code";
  return "draft";
}

export function AttachmentsPane() {
  const sessionOptions = () => Object.values(rootStore.sessions);
  const [selectedId, setSelectedId] = createSignal<string | null>(
    sessionOptions()[0]?.id ?? null,
  );
  const [refreshTick, setRefreshTick] = createSignal(0);
  const selectedSession = () => {
    const id = selectedId();
    return id ? rootStore.sessions[id] ?? null : null;
  };
  const usesManagedCopies = () => {
    const session = selectedSession();
    if (!session) return false;
    return supportedWorkerPlatform(rootStore.workers[session.worker_fp]?.os) === "win32";
  };
  const setTimeoutTracked = createTrackedTimeouts();

  const [statusMsg, setStatusMsg] = createSignal<string | null>(null);

  // Hidden-tab gate: skip the refresh while hidden; next visible tick refreshes.
  const refreshTimer = setInterval(() => { if (isPageVisible()) setRefreshTick((t) => t + 1); }, 5 * 60 * 1000);
  onCleanup(() => clearInterval(refreshTimer));

  const [entries] = createResource(
    () => [selectedId(), refreshTick()] as const,
    async ([sid]) => {
      if (!sid) return [];
      try {
        const res = await coordClient.listAttachments({ sessionId: sid });
        return res.entries;
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        setStatusMsg(`List failed: ${msg}`);
        return [];
      }
    },
  );

  async function injectPath(sid: string, absPath: string): Promise<void> {
    const admission = sendUserTerminalInput(sid, new TextEncoder().encode(absPath + " "));
    if (!admission.accepted) {
      setStatusMsg(`Inject failed: ${admission.reason}`);
      return;
    }
    const outcome = await admission.result;
    setStatusMsg(outcome.status === "accepted"
      ? `Injected ${absPath}`
      : `Inject failed: ${outcome.reason}`);
    setTimeoutTracked(() => setStatusMsg(null), 2000);
  }
  async function copyPathToClipboard(absPath: string): Promise<void> {
    // Both outcomes surface as a status chip; denial names its cause since
    // there is no other feedback channel here.
    const ok = await copyToClipboard(absPath);
    setStatusMsg(ok ? "Copied to clipboard" : "Copy failed (clipboard permission denied)");
    if (ok) setTimeoutTracked(() => setStatusMsg(null), 2000);
  }
  async function deleteAttachment(sid: string, filename: string): Promise<void> {
    try {
      await coordClient.deleteAttachment({ sessionId: sid, filename });
      setRefreshTick(refreshTick() + 1);
      setStatusMsg(`Deleted ${filename}`);
      setTimeoutTracked(() => setStatusMsg(null), 2000);
    } catch (err) {
      setStatusMsg(`Delete failed: ${err instanceof Error ? err.message : String(err)}`);
    }
  }

  return (
    <div data-testid="attachments-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <Card
        title="Attachments"
        supporting="Files dropped into the PTY land in ~/.roost/attachments/<sid>/. They're swept after 24 hours."
      >
        <div class="md-form-row">
          <Select
            label="Session"
            value={selectedId() ?? ""}
            onChange={(v) => setSelectedId(v || null)}
            class="md-input"
            options={sessionOptions().map((s) => ({
              value: s.id,
              label: `${s.kind} — ${workerPathBasename(s.worker_fp, s.cwd) || s.cwd} (${s.id.slice(0, 8)})`,
            }))}
          />
        </div>

        <div class="md-body-m" style={{ display: "inline-flex", "align-items": "center", gap: "var(--md-space-2)", color: "var(--md-sys-color-on-surface-variant)" }}>
          <Switch
            label="Use short attachment paths"
            checked={getShortPathPref()}
            onChange={(checked) => {
              setShortPathPref(checked);
              setStatusMsg(`Short paths ${checked ? "on" : "off"}`);
              setTimeoutTracked(() => setStatusMsg(null), 1500);
            }}
          />
          Use short-path {usesManagedCopies() ? "managed copies" : "symlinks"} for injected paths
        </div>

        <Show when={statusMsg()}>
          <div class="md-body-m" style={{
            padding: "var(--md-space-2) var(--md-space-3)",
            background: "var(--md-sys-color-secondary-container)",
            color: "var(--md-sys-color-on-secondary-container)",
            "border-radius": "var(--md-shape-sm)",
            display: "inline-flex",
            "align-items": "center",
            gap: "var(--md-space-2)",
            "align-self": "flex-start",
          }}>
            <Icon name="info" size="sm" />
            {statusMsg()}
          </div>
        </Show>
      </Card>

      <Card title="Files">
        <Show when={!entries.loading} fallback={
          <div class="md-body-m" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>Loading…</div>
        }>
          <Show when={(entries() ?? []).length > 0} fallback={
            <EmptyState
              icon="folder_open"
              title="No attachments yet"
              supporting="Drop a file into the terminal to attach it. The path lands here and you can re-inject it anytime."
            />
          }>
            <List contained>
              <For each={entries()}>
                {(entry) => {
                  const sid = selectedId();
                  const exp = expiryStatus(Number(entry.mtimeMs));
                  const chipBg = () =>
                    exp.state === "expired" ? "var(--md-sys-color-error)"
                    : exp.state === "warn" ? "var(--md-sys-color-tertiary-container)"
                    : "var(--md-sys-color-secondary-container)";
                  const chipFg = () =>
                    exp.state === "expired" ? "var(--md-on-primary)"
                    : exp.state === "warn" ? "var(--md-sys-color-on-tertiary-container)"
                    : "var(--md-sys-color-on-secondary-container)";
                  return (
                    <ListRow
                      leading={iconForFile(entry.filename)}
                      headline={
                        <span style={{ "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace" }}>
                          {entry.filename}
                        </span>
                      }
                      support={
                        <span style={{ display: "inline-flex", gap: "var(--md-space-3)", "align-items": "center" }}>
                          <span>{formatBytes(Number(entry.sizeBytes))}</span>
                          <span>{formatAge(Number(entry.mtimeMs))}</span>
                          <span
                            class="md-label-s"
                            style={{
                              padding: "2px 8px",
                              "border-radius": "var(--md-shape-full)",
                              background: chipBg(),
                              color: chipFg(),
                            }}
                          >
                            {exp.label}
                          </span>
                        </span>
                      }
                      trailing={
                        <>
                          <Button variant="text" icon="content_paste_go" onClick={() => sid && injectPath(sid, entry.absPath)}>
                            Inject
                          </Button>
                          <Button variant="text" icon="content_copy" onClick={() => copyPathToClipboard(entry.absPath)}>
                            Copy
                          </Button>
                          <Button
                            variant="text"
                            icon="delete_outline"
                            style={{ color: "var(--md-sys-color-error)" }}
                            onClick={() => sid && deleteAttachment(sid, entry.filename)}
                          >
                            Delete
                          </Button>
                        </>
                      }
                    />
                  );
                }}
              </For>
            </List>
          </Show>
        </Show>
      </Card>
    </div>
  );
}
