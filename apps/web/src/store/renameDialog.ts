// Module-level state for the "Rename…" dialog, opened from SessionRow's
// right-click menu (the promptRename model — a small modal, not inline).
// Carries the session id + the title to pre-fill (current custom name, or the
// auto title if none). Commit calls coordClient.sessionsRename. See
// RenameDialog.tsx (the mounted host) and sessionTitle.ts (precedence).

import { createSignal } from "solid-js";

export interface RenameDialogContext {
  currentTitle: string; // pre-fill: custom_title ?? auto title
  hasCustom: boolean; // true → offer "Reset to auto"
  headline?: string; // dialog title; default "Rename terminal"
  // Session-rename path: RenameDialogHost calls coordClient.sessionsRename(sessionId).
  sessionId?: string;
  // Generic path (folder → workspace): supply a commit fn; the host awaits it
  // instead of sessionsRename. Throw to surface an error toast. name is trimmed.
  onCommit?: (name: string) => Promise<void>;
}

const [_active, _setActive] = createSignal<RenameDialogContext | null>(null);

export const activeRenameDialog = _active;

export function openRenameDialog(ctx: RenameDialogContext): void {
  _setActive(ctx);
}

export function closeRenameDialog(): void {
  _setActive(null);
}
