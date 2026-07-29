// The one allowlist deciding who may take keyboard focus away from a terminal.
//
// CellTerminal installs two document-level guards that keep focus on the
// terminal — a mousedown PREVENT and a keydown RECOVER — and both decide with
// `target.closest(FOCUS_OWNERS)`. Everything NOT listed here (bare buttons:
// sidebar ✕/FAB, mic, nav-keys, launch FAB, tabs, toasts) is fair game to keep
// terminal focus, which is the point of the guards.
//
// Custom-element tags are listed explicitly because a click inside a SHADOW
// ROOT retargets e.target to the host: a Material text field is seen as
// <md-outlined-text-field>, which matched none of the native selectors, so the
// guard ate focus for every one of them. That shipped once and made the agent
// composer unusable — no caret and keystrokes routed to the PTY on desktop, no
// keyboard at all on mobile, because preventDefault ran on the tap.
//
// [data-testid="transcript-deck"] covers the agent transcript as a SUBTREE: it
// renders above a still-mounted TerminalDeck, so nothing inside it is ever the
// terminal's focus to keep — including controls added to it later.
//
// Lives in lib/ (not inside the component) so it is importable by a test
// without dragging the JSX runtime in. Tripwire: tests/focusOwners.test.ts.
export const FOCUS_OWNERS =
	'input, textarea, select, [contenteditable=""], [contenteditable="true"], [role="textbox"], [role="searchbox"], [role="dialog"], [role="menu"], dialog, .wterm, md-outlined-text-field, md-filled-text-field, md-outlined-select, md-filled-select, [data-testid="transcript-deck"]';
