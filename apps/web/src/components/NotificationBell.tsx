// NotificationBell — a header-level bell icon with an unread count badge,
// clicking opens a dropdown listing the attention-notification log.
//
// Two exports:
// - NotificationBellTrigger — the bell button + badge. Inlined into the
//   mobile bars (MobileDeckBar / MobileTopBar / HomeLanding) on compact and
//   into the desktop sidebar brand row (AllView).
// - NotificationBell — owns the dropdown only. Open state lives in uiStore
//   so the bar/brand-row trigger and the dropdown share one signal.
//
// Owners: App.tsx RootShell. Depends on: notifyStore (log + unreadCount +
// markAllRead + markRead + clearAll + dismissNotification), notifyPrefs,
// lib/relTime, store/selectors (activeSessionForPath for navigate).
//
// The dropdown follows the FolderRowContextMenu outside-click pattern: an
// invisible full-screen scrim at z-99 closes on click, the panel sits above it.

import { Show, For, onMount, onCleanup, createEffect, createSignal, on } from "solid-js";
import type { JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { Icon } from "./Settings/md/primitives.tsx";
import { useNavigate } from "@solidjs/router";
import {
  notifications, unreadCount, markAllRead, markRead, clearAll, dismissNotification,
  setNavigateHandler, type AttentionNotification,
} from "../lib/notifyStore.ts";
import { relTimeSince } from "../lib/relTime.ts";
import { presentationOf } from "../lib/agentStatus.ts";
import { isCompact } from "../lib/windowSizeClass.ts";
import { uiStore, toggleNotificationBell, closeNotificationBell, setBellAnchor } from "../store/uiStore.ts";
import { notifyPrefs } from "../lib/notifyPrefs.ts";
import { subscribeToPush } from "../lib/push-client.ts";

// Map notification kind → attention level for color reuse.
const KIND_LEVEL: Record<AttentionNotification["kind"], "blocked" | "done"> = {
  blocked: "blocked",
  done: "done",
  offline: "blocked",
};

// ─── Trigger — the bell button + unread badge ───────────────────────────
// Inlined into mobile bars or wrapped in a fixed-position div on desktop.
// Open state lives in uiStore so the dropdown (rendered by NotificationBell)
// stays in sync regardless of which trigger was tapped.

export function NotificationBellTrigger(props: { style?: JSX.CSSProperties; class?: string }) {
  const count = () => unreadCount();

  return (
    <button
      type="button"
      data-testid="notification-bell"
      aria-label={count() > 0 ? `Notifications — ${count()} unread` : "Notifications"}
      onClick={(e) => { e.stopPropagation(); setBellAnchor(e.currentTarget as HTMLElement); toggleNotificationBell(); }}
      class={props.class}
      style={{
        width: "36px",
        height: "36px",
        border: "none",
        background: "transparent",
        cursor: "pointer",
        display: "flex",
        "align-items": "center",
        "justify-content": "center",
        color: count() > 0 ? "var(--md-sys-color-on-surface)" : "var(--md-sys-color-on-surface-variant)",
        "border-radius": "50%",
        transition: "background 120ms",
        position: "relative",
        ...props.style,
      }}
      onMouseEnter={(e) => { e.currentTarget.style.background = "var(--md-state-hover)"; }}
      onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
    >
      <Icon name="notifications" filled={count() > 0} size="md" />
      <Show when={count() > 0}>
        <span
          data-testid="notification-badge"
          style={{
            position: "absolute",
            top: "4px",
            right: "4px",
            "min-width": "16px",
            height: "16px",
            padding: "0 4px",
            "border-radius": "8px",
            background: "var(--md-warning)",
            color: "var(--md-sys-color-on-surface)",
            "font-size": "10px",
            "font-weight": "700",
            "line-height": "16px",
            "text-align": "center",
            border: "2px solid var(--md-sys-color-surface)",
            "box-sizing": "border-box",
            animation: count() === 1 ? "df-tab-dot-pulse 1.8s ease-in-out infinite" : undefined,
          }}
        >{count() > 9 ? "9+" : count()}</span>
      </Show>
    </button>
  );
}

// ─── Dropdown — the notification list panel ───────────────────────────────

function NotificationDropdown(props: {
  onJump: (notif: AttentionNotification) => void;
  onClose: () => void;
  style?: JSX.CSSProperties;
}) {
  // OS-notification permission state — re-reads on each mount (dropdown mounts per open).
  const [perm, setPerm] = createSignal<NotificationPermission | "unsupported">(
    typeof Notification !== "undefined" ? Notification.permission : "unsupported",
  );
  const showEnable = () => notifyPrefs().desktop && perm() === "default";
  const showDenied = () => notifyPrefs().desktop && perm() === "denied";

  return (
    <>
      {/* Click-away scrim (z-99) — same pattern as FolderRowContextMenu. */}
      <div
        style={{ position: "fixed", inset: "0", "z-index": 99 }}
        onClick={(e) => { e.stopPropagation(); props.onClose(); }}
        onContextMenu={(e) => { e.preventDefault(); props.onClose(); }}
      />
      {/* Dropdown panel. */}
      <div
        data-testid="notification-dropdown"
        class="df-menu-enter"
        style={{
          position: "fixed",
          /* top, left/right set dynamically from dropdownStyle */
          "max-height": "min(480px, 70vh)",
          display: "flex",
          "flex-direction": "column",
          background: "var(--md-sys-color-surface-container-high)",
          border: "1px solid var(--md-sys-color-outline-variant)",
          "border-radius": "var(--md-shape-md)",
          "box-shadow": "var(--md-elev-3)",
          overflow: "hidden",
          "z-index": 100,
          ...props.style,
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div style={{
          display: "flex", "align-items": "center", "justify-content": "space-between",
          padding: "12px 16px", "border-bottom": "1px solid var(--md-sys-color-outline-variant)",
        }}>
          <span style={{ "font-size": "14px", "font-weight": "600", color: "var(--md-sys-color-on-surface)" }}>
            Notifications
          </span>
          <Show when={notifications().length > 0}>
            <button
              type="button"
              data-testid="notification-clear-all"
              onClick={() => clearAll()}
              style={{
                border: "none", background: "transparent", cursor: "pointer",
                "font-size": "12px", color: "var(--md-sys-color-primary)",
                padding: "4px 8px", "border-radius": "var(--md-shape-xs)",
              }}
            >Clear all</button>
          </Show>
        </div>

        {/* OS-notification permission prompt */}
        <Show when={showEnable()}>
          <button
            type="button"
            data-testid="notification-enable-permission"
            onClick={async () => {
              try { await subscribeToPush(); } catch { /* error surfaced via Settings → Notifications */ }
              setPerm(typeof Notification !== "undefined" ? Notification.permission : "unsupported");
            }}
            style={{
              width: "100%",
              display: "flex",
              "align-items": "center",
              gap: "10px",
              padding: "10px 16px",
              border: "none",
              background: "transparent",
              cursor: "pointer",
              "text-align": "left",
              "border-bottom": "1px solid var(--md-sys-color-outline-variant)",
            }}
            onMouseEnter={(e) => { e.currentTarget.style.background = "var(--md-state-hover)"; }}
            onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
          >
            <Icon name="notifications" size="sm" />
            <span style={{ "font-size": "13px", color: "var(--md-sys-color-primary)" }}>
              Enable system notifications
            </span>
          </button>
        </Show>
        <Show when={showDenied()}>
          <div
            style={{
              display: "flex",
              "align-items": "center",
              gap: "10px",
              padding: "10px 16px",
              "border-bottom": "1px solid var(--md-sys-color-outline-variant)",
            }}
          >
            <Icon name="notifications" size="sm" filled={false} />
            <span style={{ "font-size": "13px", color: "var(--md-sys-color-on-surface-variant)" }}>
              Blocked — enable notifications for Roost in your browser's site settings.
            </span>
          </div>
        </Show>

        {/* List */}
        <div style={{ overflow: "auto", "flex": "1" }}>
          <Show when={notifications().length === 0}
            fallback={
              <For each={notifications()}>
                {(n) => {
                  const level = KIND_LEVEL[n.kind];
                  const visual = presentationOf(level);
                  return (
                    <div
                      role="button"
                      tabindex="0"
                      data-testid={`notification-row-${n.id}`}
                      onClick={() => props.onJump(n)}
                      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); props.onJump(n); } }}
                      style={{
                        width: "100%",
                        display: "flex",
                        "align-items": "flex-start",
                        gap: "10px",
                        padding: "10px 16px",
                        border: "none",
                        background: "transparent",
                        cursor: "pointer",
                        "text-align": "left",
                        "border-bottom": "1px solid var(--md-sys-color-outline-variant)",
                      }}
                      onMouseEnter={(e) => { e.currentTarget.style.background = "var(--md-state-hover)"; }}
                      onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
                    >
                      {/* Status dot */}
                      <span style={{
                        "flex-shrink": 0,
                        width: "10px",
                        height: "10px",
                        "border-radius": "50%",
                        "margin-top": "4px",
                        background: visual.color,
                      }} />
                      {/* Content */}
                      <div style={{ "flex": 1, "min-width": 0, display: "flex", "flex-direction": "column", gap: "2px" }}>
                        <span style={{
                          "font-size": "13px",
                          "font-weight": n.read ? 400 : 600,
                          color: "var(--md-sys-color-on-surface)",
                          "white-space": "nowrap",
                          overflow: "hidden",
                          "text-overflow": "ellipsis",
                        }}>{n.message}</span>
                        <span style={{
                          "font-size": "11px",
                          color: "var(--md-sys-color-on-surface-variant)",
                          "white-space": "nowrap",
                          overflow: "hidden",
                          "text-overflow": "ellipsis",
                        }}>{n.cwd}</span>
                      </div>
                      {/* Time + dismiss */}
                      <div style={{
                        "flex-shrink": 0,
                        display: "flex",
                        "flex-direction": "column",
                        "align-items": "flex-end",
                        gap: "4px",
                      }}>
                        <span style={{ "font-size": "11px", color: "var(--md-sys-color-on-surface-variant)" }}>
                          {relTimeSince(n.ts)}
                        </span>
                        <button
                          type="button"
                          aria-label="Dismiss"
                          onClick={(e) => { e.stopPropagation(); dismissNotification(n.id); }}
                          style={{
                            border: "none", background: "transparent", cursor: "pointer",
                            "font-size": "12px", color: "var(--md-sys-color-on-surface-variant)",
                            padding: "2px 4px", "border-radius": "var(--md-shape-xs)",
                            "line-height": "1",
                          }}
                        >✕</button>
                      </div>
                    </div>
                  );
                }}
              </For>
            }
          >
            <div style={{
              padding: "32px 16px",
              "text-align": "center",
              color: "var(--md-sys-color-on-surface-variant)",
              "font-size": "13px",
            }}>
              No notifications yet
            </div>
          </Show>
        </div>
      </div>
    </>
  );
}

// ─── NotificationBell — owns the dropdown + desktop floating trigger ──────

export function NotificationBell() {
  const navigate = useNavigate();

  // Register the navigate handler so toast/OS-notification jump-to-session works,
  // and route service-worker notificationclick messages (an already-open tab was
  // focused) to the same navigation.
  const onSwMessage = (e: MessageEvent) => {
    const data = e.data;
    if (data && data.type === "roost-navigate" && typeof data.sessionId === "string") {
      navigate(`/s/${data.sessionId}`);
    }
  };
  onMount(() => {
    setNavigateHandler((sessionId: string) => navigate(`/s/${sessionId}`));
    navigator.serviceWorker?.addEventListener("message", onSwMessage);
  });
  onCleanup(() => {
    setNavigateHandler(null);
    navigator.serviceWorker?.removeEventListener("message", onSwMessage);
  });

  // Mark all read when the dropdown opens (was in toggle() before).
  createEffect(on(() => uiStore.notificationBellOpen, (open) => {
    if (open && unreadCount() > 0) markAllRead();
  }, { defer: true }));

  function jumpTo(notif: AttentionNotification) {
    navigate(`/s/${notif.sessionId}`);
    markRead(notif.id);
    closeNotificationBell();
  }

  const open = () => uiStore.notificationBellOpen;

  // Anchor the dropdown to whichever trigger was tapped. The trigger's
  // bottom + 4px gives a small gap; left-align if the dropdown fits,
  // otherwise right-align to stay in viewport. Fall back to a hardcoded
  // position if no anchor is present (e.g. opened before first click).
  const dropdownStyle = (): JSX.CSSProperties => {
    const el = uiStore.bellAnchorEl;
    const w = isCompact() ? Math.min(360, window.innerWidth - 16) : 360;
    if (!el) return { top: "56px", right: "12px", width: `${w}px` };
    const r = el.getBoundingClientRect();
    if (r.width === 0) return { top: "56px", right: "12px", width: `${w}px` };
    const top = `${r.bottom + 4}px`;
    const fitsLeft = r.left + w <= window.innerWidth - 8;
    return fitsLeft
      ? { top, left: `${r.left}px`, width: `${w}px` }
      : { top, right: `${window.innerWidth - r.right}px`, width: `${w}px` };
  };
  return (
    <Portal mount={document.body}>
      <Show when={open()}>
        <NotificationDropdown
          onJump={jumpTo}
          onClose={closeNotificationBell}
          style={dropdownStyle()}
        />
      </Show>
    </Portal>
  );
}
