// "Copy on select" for the terminal — the xterm/tmux habit of putting a
// selection on the clipboard the moment the drag ends. Default OFF: it silently
// overwrites the system clipboard, which is surprising if you did not ask for it.
//
// Persisted per device. Read by CellTerminal's pointerup/keyup handler; the
// toggle lives in Settings → Terminal.

import { createSignal } from "solid-js";

const KEY = "roost.copyOnSelect";

function readStored(): boolean {
  if (typeof localStorage === "undefined") return false;
  return localStorage.getItem(KEY) === "1";
}

const [copyOnSelect, _setCopyOnSelect] = createSignal(readStored());
export { copyOnSelect };

export function setCopyOnSelect(on: boolean): void {
  _setCopyOnSelect(on);
  try { localStorage.setItem(KEY, on ? "1" : "0"); } catch { /* private mode / quota */ }
}
