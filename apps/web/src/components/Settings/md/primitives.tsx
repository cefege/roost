// M3 primitives shared across the settings panes. Class-based, so the
// CSS lives in tokens.css and the JSX stays terse. Anything more
// component-shaped (dialog, switch, ripple) defers to @material/web
// custom elements imported lazily by the consuming pane.
//
// Owners: SettingsRoot.tsx + every pane under Settings/.
// Depends on: ./tokens.css for the class definitions.
//
// Barrel: one-component-per-file (CLAUDE.md L376). Each primitive lives in its
// own sibling under ./md/; this file re-exports them so
// `import { Card, ... } from "./md/primitives.tsx"` resolves unchanged. The
// @material/web registrations stay here too so importing ANY primitive from the
// barrel registers every md custom element, exactly as the single-file version
// did (ES-module singletons → no double-define even though each sibling also
// imports the ones it uses).

import "@material/web/switch/switch.js";
import "@material/web/checkbox/checkbox.js";
import "@material/web/button/filled-button.js";
import "@material/web/button/filled-tonal-button.js";
import "@material/web/button/text-button.js";
import "@material/web/iconbutton/icon-button.js";
import "@material/web/textfield/outlined-text-field.js";
import "@material/web/dialog/dialog.js";
import "@material/web/chips/assist-chip.js";
import "@material/web/select/outlined-select.js";
import "@material/web/select/select-option.js";

export { Switch } from "./Switch.tsx";
export { IconButton } from "./IconButton.tsx";
export { TextField } from "./TextField.tsx";
export { Dialog } from "./Dialog.tsx";
export { Chip } from "./Chip.tsx";
export { Select } from "./Select.tsx";
export { Checkbox } from "./Checkbox.tsx";
export { Icon } from "./Icon.tsx";
export { Card } from "./Card.tsx";
export { SectionTitle } from "./SectionTitle.tsx";
export { List } from "./List.tsx";
export { ListRow } from "./ListRow.tsx";
export { MetricTile } from "./MetricTile.tsx";
export { EmptyState } from "./EmptyState.tsx";
export { Surface } from "./Surface.tsx";
export { StatusDot } from "./StatusDot.tsx";
export { Sheet } from "./Sheet.tsx";
export { Button } from "./Button.tsx";
