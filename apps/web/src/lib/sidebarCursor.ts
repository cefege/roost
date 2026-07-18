// Keyboard cursor for the flat sidebar list. Holds the rendered session
// order (published by FolderList) + the current cursor index, so the
// global key handler (keyboardShortcuts.ts ↑/↓/⏎ branch) and the rendered
// rows agree on which row is highlighted. The cursor is a SEPARATE state
// from "selected" — selected = URL match (data-selected), cursor =
// keyboard highlight (data-cursor). Never overload the two (L11 /
// feedback_selected_means_url_match_not_has_children).
//
// FolderList owns the router (useNavigate) and registers an activate
// callback here so this module stays free of router/store imports.
// Depends on: solid-js createSignal only.

import { createSignal } from "solid-js";

const [_ids, _setIds] = createSignal<string[]>([]);
const [_cursor, _setCursor] = createSignal(-1); // -1 = no row highlighted

let _activate: ((id: string) => void) | null = null;

/** FolderList registers the open-session action (navigate + push MRU).
 *  Pass null on cleanup. */
export function setActivateHandler(fn: ((id: string) => void) | null): void {
  _activate = fn;
}

/** FolderList publishes its rendered row order every recompute so
 *  keyboard nav matches what's on screen. Clamps the cursor into range.
 *  Skip when order is unchanged — the default ref-equality signal would
 *  otherwise echo-invalidate every row's data-cursor after each rebuild. */
export function setOrderedSessionIds(ids: string[]): void {
  const prev = _ids();
  if (prev.length === ids.length && prev.every((id, i) => id === ids[i])) return;
  _setIds(ids);
  if (_cursor() >= ids.length) _setCursor(ids.length - 1);
}

export function cursorSessionId(): string | null {
  const ids = _ids();
  const c = _cursor();
  return c >= 0 && c < ids.length ? ids[c] : null;
}

/** Move the cursor by delta, clamped to [0, len-1]. From -1 (no
 *  selection), ↓ lands on the first row and ↑ on the last. */
export function moveCursor(delta: number): void {
  const len = _ids().length;
  if (len === 0) { _setCursor(-1); return; }
  const cur = _cursor();
  if (cur < 0) { _setCursor(delta > 0 ? 0 : len - 1); return; }
  _setCursor(Math.min(len - 1, Math.max(0, cur + delta)));
}

/** Open the session under the cursor via the registered activate handler. */
export function activateCursor(): void {
  const id = cursorSessionId();
  if (id && _activate) _activate(id);
}
