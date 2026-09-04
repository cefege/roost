// Per-browser notification preferences. Desktop delivery defaults off because
// enabling it must be an explicit user gesture that grants permission and
// completes the coordinator subscription first.

import { createSignal } from "solid-js";

const STORAGE_KEY = "roost.notifications.prefs.v2";

export interface NotifyPrefs {
  inApp: boolean;
  desktop: boolean;
  titleBadge: boolean;
  blockedSound: boolean;
  doneSound: boolean;
}

const DEFAULTS: NotifyPrefs = {
  inApp: true,
  desktop: false,
  titleBadge: true,
  blockedSound: false,
  doneSound: false,
};

function parse(raw: string | null): NotifyPrefs {
  if (!raw) return { ...DEFAULTS };
  try {
    const value = JSON.parse(raw) as Record<string, unknown>;
    const next = { ...DEFAULTS };
    for (const key of Object.keys(DEFAULTS) as Array<keyof NotifyPrefs>) {
      if (typeof value[key] === "boolean") next[key] = value[key] as boolean;
    }
    return next;
  } catch {
    return { ...DEFAULTS };
  }
}

function load(): NotifyPrefs {
  try {
    return typeof localStorage === "undefined"
      ? { ...DEFAULTS }
      : parse(localStorage.getItem(STORAGE_KEY));
  } catch {
    return { ...DEFAULTS };
  }
}

const [notifyPrefs, setPrefs] = createSignal<NotifyPrefs>(load());
export { notifyPrefs };

export function setNotifyPref<K extends keyof NotifyPrefs>(key: K, value: NotifyPrefs[K]): void {
  setPrefs((previous) => {
    if (previous[key] === value) return previous;
    const next = { ...previous, [key]: value };
    try { localStorage.setItem(STORAGE_KEY, JSON.stringify(next)); } catch { /* unavailable */ }
    return next;
  });
}

/** Persist desktop=true only after permission + subscription complete. */
export async function enableDesktopNotifications(
  subscribe: () => Promise<void>,
): Promise<void> {
  await subscribe();
  setNotifyPref("desktop", true);
}

export async function disableDesktopNotifications(
  unsubscribe: () => Promise<void>,
): Promise<void> {
  setNotifyPref("desktop", false);
  await unsubscribe();
}

if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
  window.addEventListener("storage", (event) => {
    if (event.key === STORAGE_KEY) setPrefs(parse(event.newValue));
  });
}

/** Reset Push-related browser state when its owning account signs out. */
export function clearNotificationStateForLogout(): void {
  setPrefs({ ...DEFAULTS });
  try { localStorage.removeItem(STORAGE_KEY); } catch { /* unavailable */ }
}

export function resetNotifyPrefsForTest(): void {
  clearNotificationStateForLogout();
}
