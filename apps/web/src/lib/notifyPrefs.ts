// Notification preferences — client-only UI prefs, persisted to localStorage.
// Follows the micOnDesktop.ts pattern: a single reactive signal, getters read
// `notifyPrefs().<key>`, `setNotifyPref(key, value)` persists + updates.
// Toggled from Settings → Notifications (NotificationsPane.tsx).

import { createSignal } from "solid-js";

const STORAGE_KEY = "roost.notifications.prefs";

export interface NotifyPrefs {
  toast: boolean;        // in-app toast on transition. Default: true.
  desktop: boolean;      // OS notification when tab backgrounded. Default: true.
  titleBadge: boolean;   // "(N) Roost" document.title prefix. Default: true.
  sound: boolean;        // audio cue on needs-input only. Default: false.
  soundOnDone: boolean;  // audio cue on done too. Default: false.
}

const DEFAULTS: NotifyPrefs = {
  toast: true,
  desktop: true,
  titleBadge: true,
  sound: false,
  soundOnDone: false,
};

function load(): NotifyPrefs {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<NotifyPrefs>;
    return { ...DEFAULTS, ...parsed };
  } catch {
    return { ...DEFAULTS };
  }
}

const [prefs, setPrefs] = createSignal<NotifyPrefs>(load());

/** Reactive accessor — read inside JSX/effects to toggle live. */
export { prefs as notifyPrefs };

/** Persist + flip one pref. Applies immediately (reactive signal). */
export function setNotifyPref<K extends keyof NotifyPrefs>(key: K, value: NotifyPrefs[K]): void {
  setPrefs((prev) => {
    const next = { ...prev, [key]: value };
    try { localStorage.setItem(STORAGE_KEY, JSON.stringify(next)); } catch { /* quota / privacy */ }
    return next;
  });
}
