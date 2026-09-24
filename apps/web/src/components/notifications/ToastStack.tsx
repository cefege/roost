// ToastStack — maps the toast signal onto dock children. Geometry belongs to
// NotificationDock; this module only turns store rows into cards.
// Reads store/toastStore.ts; rendered by NotificationDock.tsx.

import { For } from "solid-js";
import { toasts } from "../../store/toastStore.ts";
import { ToastCard } from "./ToastCard.tsx";

export function ToastStack() {
  return <For each={toasts()}>{(toast) => <ToastCard toast={toast} />}</For>;
}
