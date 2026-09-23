// "New browser paired" notice for every authorized browser. Sync's volatile
// PairCompleted delta (store/sync-frame.ts) and the approver's own status poll
// (PairApprovalProvider) both announce through here, so one pairing toasts at
// most once per document whichever signal lands first. Depends on toastStore.

import { addToast } from "../store/toastStore.ts";

export interface PairedBrowserNotice {
  ephemeralId: string;
  label: string;
}

export interface PairedBrowserDescription {
  label: string;
  clientBrowser: string;
  clientOs: string;
  city: string;
  region: string;
  countryCode: string;
}

const ANNOUNCED_ID_LIMIT = 32;
const UNKNOWN_BROWSER_LABEL = "Unknown browser";
// Insertion order is eviction order: the oldest id leaves first at the bound.
const announcedEphemeralIds = new Set<string>();

export function announcePairedBrowser(notice: PairedBrowserNotice): void {
  if (announcedEphemeralIds.has(notice.ephemeralId)) return;
  announcedEphemeralIds.add(notice.ephemeralId);
  if (announcedEphemeralIds.size > ANNOUNCED_ID_LIMIT) {
    const [oldest] = announcedEphemeralIds;
    announcedEphemeralIds.delete(oldest!);
  }
  addToast(`New browser paired: ${notice.label.trim() || UNKNOWN_BROWSER_LABEL}`, "ok");
}

/** "Chrome on macOS · Berlin": parsed browser/OS first, the requester's own
 *  label when neither was parsed, then the most specific known place. */
export function formatPairedBrowserLabel(completed: PairedBrowserDescription): string {
  const browser = completed.clientBrowser.trim();
  const os = completed.clientOs.trim();
  const device = browser && os
    ? `${browser} on ${os}`
    : browser || os || completed.label.trim() || UNKNOWN_BROWSER_LABEL;
  const place = [completed.city, completed.region, completed.countryCode]
    .map((part) => part.trim())
    .find((part) => part.length > 0);
  return place ? `${device} · ${place}` : device;
}
