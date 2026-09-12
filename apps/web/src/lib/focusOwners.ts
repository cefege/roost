// The one allowlist deciding which controls may claim keyboard focus from a terminal.
// CellTerminal's document guards test event targets against this selector.
// Native and Kobalte text, dialog, and popup owners may retain focus.
// Generic buttons remain excluded so terminal focus stays stable.

export const FOCUS_OWNERS =
	'input, textarea, select, [contenteditable=""], [contenteditable="true"], [role="textbox"], [role="searchbox"], [role="dialog"], [role="menu"], [role="listbox"], [role="combobox"], dialog, .wterm';
