// M3 primitive barrel shared across settings and the wider SPA.
// Each component lives in its own sibling and retains this stable re-export surface.
// SettingsRoot owns shell utilities; component-owned CSS travels with its primitive.
// Importing the barrel registers every Material custom element exactly once.

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
