// document.title unread counter — reacts to unreadCount() from notifyStore and
// prefixes the tab title with "(N) " when there are unread notifications.
// No existing code writes document.title (grep-confirmed); this is the sole
// owner. On disable (notifyPrefs().titleBadge === false) or zero unread, the
// title reverts to the base.

import { createRoot, createEffect } from "solid-js";
import { unreadCount } from "./notifyStore.ts";
import { notifyPrefs } from "./notifyPrefs.ts";

const BASE_TITLE = "Roost";

// Cache the original title at module load (before any effect runs) in case
// the host page sets a custom title.
const _originalTitle = typeof document !== "undefined" ? document.title : BASE_TITLE;

createRoot(() => {
  createEffect(() => {
    const badgeOn = notifyPrefs().titleBadge;
    const n = unreadCount();
    if (!badgeOn || n === 0) {
      document.title = _originalTitle || BASE_TITLE;
      return;
    }
    document.title = `(${n}) ${_originalTitle || BASE_TITLE}`;
  });
});
