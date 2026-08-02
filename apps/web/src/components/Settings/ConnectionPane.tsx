// ConnectionPane — pick which coord this browser talks to (Settings → Connection).
// The active coord URL lives in localStorage["roost.coordinatorUrl"], read at
// load by connect.ts::coordBase(). Empty = same-origin (the coord that served
// this UI) = the default, so an untouched install behaves exactly as before.
// This pane is the UI over that key: a saved list of named coords + switch.
// Switching writes the key and reloads (the Connect transport is built once at
// module load, so a reload is how a new baseUrl takes effect).
// Callers: SettingsRoot.tsx.

import { createSignal, For, Show } from "solid-js";
import { Card, Button, TextField, ListRow, EmptyState, Icon } from "./md/primitives.tsx";

// Same key connect.ts::coordBase() reads. Keep in sync.
const ACTIVE_KEY = "roost.coordinatorUrl";
const LIST_KEY = "roost.coords";

interface SavedCoord {
  name: string;
  url: string; // full origin, no trailing slash, e.g. https://mac.ts.net:4102
}

function loadSaved(): SavedCoord[] {
  try {
    const raw = localStorage.getItem(LIST_KEY);
    const parsed = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? parsed.filter((c) => c && typeof c.url === "string") : [];
  } catch {
    return [];
  }
}

function persistSaved(list: SavedCoord[]): void {
  localStorage.setItem(LIST_KEY, JSON.stringify(list));
}

function activeUrl(): string {
  return localStorage.getItem(ACTIVE_KEY) ?? "";
}

// Write the active key + reload so connect.ts rebuilds the transport at baseUrl.
function switchTo(url: string): void {
  if (url) localStorage.setItem(ACTIVE_KEY, url);
  else localStorage.removeItem(ACTIVE_KEY);
  location.reload();
}

// Normalize a typed URL: trim, strip trailing slash. Returns null if it isn't
// an http(s) origin (we don't want to silently point the transport at garbage).
function normalizeUrl(raw: string): string | null {
  const trimmed = raw.trim().replace(/\/+$/, "");
  if (!/^https?:\/\//.test(trimmed)) return null;
  return trimmed;
}

export function ConnectionPane() {
  const [saved, setSaved] = createSignal<SavedCoord[]>(loadSaved());
  const [name, setName] = createSignal("");
  const [url, setUrl] = createSignal("");
  const [error, setError] = createSignal("");

  const active = activeUrl(); // read once; switching reloads the page

  function addCoord() {
    const normalized = normalizeUrl(url());
    if (!normalized) {
      setError("Enter a full address like https://your-mac.ts.net:4102");
      return;
    }
    const label = name().trim() || normalized;
    const next = [...saved().filter((c) => c.url !== normalized), { name: label, url: normalized }];
    persistSaved(next);
    setSaved(next);
    setName("");
    setUrl("");
    setError("");
  }

  function removeCoord(target: string) {
    const next = saved().filter((c) => c.url !== target);
    persistSaved(next);
    setSaved(next);
  }

  return (
    <div data-testid="settings-connection-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <Card
        title="Connection"
        supporting="Which coordinator this browser talks to. The default is the server that opened this page. Add direct, non-Access addresses such as localhost or a tailnet URL."
      >
        <p class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>
          Cloudflare Access addresses must be opened directly in the browser, not configured here.
        </p>
        <ListRow
          leading={<Icon name="home" />}
          headline="This server (default)"
          support={active === "" ? "Connected" : "Same origin that served this page"}
          selected={active === ""}
          trailing={active === "" ? <Icon name="check" /> : <Button variant="text" onClick={() => switchTo("")}>Use</Button>}
          testId="coord-default"
        />
        <For each={saved()}>
          {(coord) => (
            <ListRow
              leading={<Icon name="dns" />}
              headline={coord.name}
              support={coord.url + (active === coord.url ? " · Connected" : "")}
              selected={active === coord.url}
              trailing={
                <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
                  <Show when={active !== coord.url}>
                    <Button variant="text" onClick={() => switchTo(coord.url)}>Use</Button>
                  </Show>
                  <Show when={active === coord.url}><Icon name="check" /></Show>
                  <Button variant="text" icon="delete" aria-label="Remove" onClick={() => removeCoord(coord.url)} />
                </div>
              }
              testId={`coord-row-${coord.url}`}
            />
          )}
        </For>
        <Show when={saved().length === 0}>
          <EmptyState
            icon="lan"
            title="No saved coordinators"
            supporting="Add one below to reach a coord that isn't the server that served this page."
          />
        </Show>
      </Card>

      <Card title="Add a coordinator" supporting="Give it a name and its full address. The address must be reachable from this device.">
        <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-3)" }}>
          <TextField value={name()} onInput={setName} label="Name (optional)" placeholder="Home Macs" />
          <TextField value={url()} onInput={setUrl} label="Address" placeholder="https://your-mac.ts.net:4102" />
          <Show when={error()}>
            <div class="md-body-s" style={{ color: "var(--md-sys-color-error)" }}>{error()}</div>
          </Show>
          <div>
            <Button variant="filled" icon="add" onClick={addCoord}>Add</Button>
          </div>
        </div>
      </Card>
    </div>
  );
}
