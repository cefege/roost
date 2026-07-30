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
// <md-outlined-text-field>, which matched none of the native selectors. The
// guards must recognize those hosts so their form controls can receive focus.
// Lives in lib/ (not inside the component) so it is importable by a test
// without dragging the JSX runtime in. Tripwire: tests/focusOwners.test.ts.
export const FOCUS_OWNERS =
	'input, textarea, select, [contenteditable=""], [contenteditable="true"], [role="textbox"], [role="searchbox"], [role="dialog"], [role="menu"], dialog, .wterm, md-outlined-text-field, md-filled-text-field, md-outlined-select, md-filled-select';
