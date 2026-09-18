// The one predicate for "a non-pointer device is driving DOM focus".
// TV remotes and game controllers both suppress PTY auto-focus, make the
// terminal scroll box focusable, and hand the arrows to spatialNavigation.
// Callers: spatialNavigation, keyboardShortcuts, CellTerminal + its
// interaction/lifecycle/renderer helpers. Depends on: lib/tvMode, lib/padMode.

import { tvModeActive } from "./tvMode.ts";
import { padModeActive } from "./padMode.ts";

export function directionalInputActive(): boolean {
	return tvModeActive() || padModeActive();
}
