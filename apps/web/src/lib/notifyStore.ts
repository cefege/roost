// Attention notification store — the core engine. Watches rootStore.sessions
// and rootStore.claude_status for agent-status transitions (working → idle /
// needs-input) and emits notifications. The notification log is a reactive
// signal (most-recent-first); side-effects (toast + OS notification + sound)
// fan out via _dispatchNotification (step 4 wires that in).
//
// This builds on the existing attention infrastructure:
// - liveStatus() (attention.ts) = s.agent?.status ?? rootStore.claude_status[id]
// - activeSessionForPath (selectors.ts) = the URL-active session
// - isPageVisible (pageVisible.ts) = tab-visibility signal
// - sessionTitle (sessionTitle.ts) = display name for a session
//
// No new attention-detection logic — just a transition wrapper over existing
// predicates. Pure web-side addition; no worker/proto/wire changes.

import { createRoot, createSignal, createMemo, createEffect, batch } from "solid-js";
import type { Session } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { liveStatus } from "./attention.ts";
import { activeSessionForPath } from "../store/selectors.ts";
import { isPageVisible } from "./pageVisible.ts";
import { sessionTitle } from "./sessionTitle.ts";
import { isClaudeSession } from "./isClaudeSession.ts";
import { notifyPrefs } from "./notifyPrefs.ts";
import { pushAttentionToast } from "./attentionToastStore.ts";

// ─── Notification record ───────────────────────────────────────────────────

export interface AttentionNotification {
  id: number;
  sessionId: string;
  sessionTitle: string;
  workerFp: string;
  cwd: string;
  kind: "done" | "blocked" | "offline";
  message: string;
  ts: number;
  read: boolean;
}

// ─── Store ──────────────────────────────────────────────────────────────────

const MAX_NOTIFICATIONS = 50;
let _nextId = 1;

const [notifications, setNotifications] = createSignal<AttentionNotification[]>([]);

/** Reactive: the notification log, most-recent-first. */
export { notifications };

export const unreadCount = createRoot(() =>
  createMemo(() => notifications().filter((n) => !n.read).length),
);

// ─── Persist read-state to localStorage ─────────────────────────────────────

const READ_KEY = "roost.notifications.read";

function loadReadIds(): Set<number> {
  try {
    const raw = localStorage.getItem(READ_KEY);
    if (!raw) return new Set();
    return new Set(JSON.parse(raw) as number[]);
  } catch { return new Set(); }
}

function persistReadIds(): void {
  try {
    const ids = notifications().filter((n) => n.read).map((n) => n.id);
    localStorage.setItem(READ_KEY, JSON.stringify(ids));
  } catch { /* quota / privacy */ }
}

// On module load, mark any persisted-as-read notifications as read.
// (Only matters if we persist the full log in the future; for now we persist
// only read-ids so a reload doesn't re-notify. The log starts empty on reload.)

// ─── Public API ─────────────────────────────────────────────────────────────

export function markAllRead(): void {
  batch(() => {
    setNotifications((prev) => prev.map((n) => ({ ...n, read: true })));
    persistReadIds();
  });
}

export function markRead(id: number): void {
  setNotifications((prev) => {
    const idx = prev.findIndex((n) => n.id === id);
    if (idx < 0 || prev[idx].read) return prev;
    const next = [...prev];
    next[idx] = { ...next[idx], read: true };
    return next;
  });
  persistReadIds();
}

export function clearAll(): void {
  setNotifications([]);
  persistReadIds();
}

export function dismissNotification(id: number): void {
  setNotifications((prev) => prev.filter((n) => n.id !== id));
  persistReadIds();
}

// ─── Navigate handler (registered from App.tsx — see keyboardShortcuts.ts pattern) ──

let _navigateHandler: ((sessionId: string) => void) | null = null;

export function setNavigateHandler(fn: ((sessionId: string) => void) | null): void {
  _navigateHandler = fn;
}

// ─── Transition classification ──────────────────────────────────────────────

function classifyTransition(prev: string, next: string): AttentionNotification["kind"] | null {
  // Any transition involving "unknown" = no notification (detector has no opinion).
  if (prev === "unknown" || next === "unknown") return null;

  // Running → idle/done = finished working (the primary use case).
  if ((prev === "running" || prev === "running-workflow") &&
      (next === "idle" || next === "done")) return "done";

  // Running → needs-input = blocked, waiting on you (highest urgency).
  if ((prev === "running" || prev === "running-workflow") && next === "needs-input")
    return "blocked";

  // Needs-input → idle/done = you answered and it finished.
  if (prev === "needs-input" && (next === "idle" || next === "done")) return "done";

  // Needs-input → running = resumed working (user acted) — no notification.
  // Any → running = started working — no notification (calm state).
  // idle/done → same = no change.
  return null;
}

// ─── Emit ───────────────────────────────────────────────────────────────────

function emitNotification(s: Session, kind: AttentionNotification["kind"]): void {
  const title = sessionTitle(s);
  const msg = kind === "blocked" ? `${title} needs your input`
            : kind === "offline" ? `${title} went offline`
            : `${title} finished`;
  const n: AttentionNotification = {
    id: _nextId++,
    sessionId: s.id,
    sessionTitle: title,
    workerFp: s.worker_fp,
    cwd: s.cwd,
    kind,
    message: msg,
    ts: Date.now(),
    read: false,
  };
  setNotifications((prev) => [n, ...prev].slice(0, MAX_NOTIFICATIONS));
  _dispatchNotification(n);
}

// ─── Side-effect fan-out (step 4) ───────────────────────────────────────────

function _playCue(kind: AttentionNotification["kind"]): void {
  // Web Audio synthesized tone — no asset file. Lazy-init AudioContext.
  // Blocked = two ascending notes (alerting); done = one soft note.
  try {
    const Ctx = window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!Ctx) return;
    // Reuse a single context for low latency.
    if (!_audioCtx) {
      try { _audioCtx = new Ctx(); } catch { return; }
    }
    const ctx = _audioCtx;
    if (ctx.state === "suspended") { void ctx.resume().catch(() => {}); }
    const now = ctx.currentTime;
    const playNote = (freq: number, start: number, dur: number, gain: number): void => {
      const osc = ctx.createOscillator();
      const g = ctx.createGain();
      osc.frequency.value = freq;
      osc.type = "sine";
      g.gain.setValueAtTime(0, now + start);
      g.gain.linearRampToValueAtTime(gain, now + start + 0.01);
      g.gain.exponentialRampToValueAtTime(0.001, now + start + dur);
      osc.connect(g).connect(ctx.destination);
      osc.start(now + start);
      osc.stop(now + start + dur + 0.05);
    };
    if (kind === "blocked") {
      playNote(660, 0, 0.12, 0.15);
      playNote(880, 0.14, 0.15, 0.15);
    } else {
      playNote(523, 0, 0.15, 0.10);
    }
  } catch { /* AudioContext unavailable — silent no-op */ }
}

let _audioCtx: AudioContext | null = null;

function _dispatchNotification(n: AttentionNotification): void {
  const prefs = notifyPrefs();

  // (a) In-app top-right attention card (see AttentionToasts.tsx).
  if (prefs.toast) pushAttentionToast(n);

  // (c) Sound — only on blocked (needs-input), unless soundOnDone.
  if (prefs.sound && (n.kind === "blocked" || (n.kind === "done" && prefs.soundOnDone))) {
    _playCue(n.kind);
  }
}

// ─── Transition detector — app-lifetime singleton ────────────────────────────

// Per-session previous-status tracker. Module-level so it survives re-renders.
const _prevStatus = new Map<string, string>();

createRoot(() => {
  createEffect(() => {
    // Iterate all sessions reactively — Solid tracks each
    // sessions[id].agent?.status and claude_status[id] access.
    const sessions = rootStore.sessions;
    const liveIds = new Set<string>();

    for (const s of Object.values(sessions)) {
      if (s.status !== "open") continue;
      if (!isClaudeSession(s)) continue;
      liveIds.add(s.id);

      const status = liveStatus(s) ?? "unknown";
      const prev = _prevStatus.get(s.id) ?? "unknown";
      _prevStatus.set(s.id, status);

      // First sighting — record only, don't notify.
      if (prev === "unknown") continue;

      // Suppress if user is currently viewing this session.
      if (isPageVisible() && activeSessionForPath(location.pathname)?.id === s.id) continue;

      const transition = classifyTransition(prev, status);
      if (!transition) continue;
      emitNotification(s, transition);
    }

    // Clean up stale _prevStatus entries for closed sessions.
    for (const id of _prevStatus.keys()) {
      if (!liveIds.has(id)) _prevStatus.delete(id);
    }
  });
});
