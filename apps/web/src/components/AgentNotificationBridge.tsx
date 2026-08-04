// Router-scoped coding-agent notification bridge. Status detection stays on
// the worker; this component only classifies ordered post-baseline transitions
// and delivers browser-profile UI effects.

import { useLocation, useNavigate } from "@solidjs/router";
import { createEffect, onCleanup, onMount } from "solid-js";
import type { AgentStatus } from "@roost/shared/wire";
import { SessionId } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { subscribeAgentStatus } from "../store/agent-status.ts";
import { activeSessionForPath } from "../store/selectors.ts";
import {
  markAgentSeen,
  seenAgentRevision,
  startAgentSeenPersistence,
} from "../lib/agentSeen.ts";
import { pageVisible, isPageVisible } from "../lib/pageVisible.ts";
import { notifyPrefs } from "../lib/notifyPrefs.ts";
import { addToast } from "../lib/toastStore.ts";
import { ensurePushSubscription } from "../lib/push-client.ts";
import { sessionTitle } from "../lib/sessionTitle.ts";
import { terminalHref } from "../lib/terminalHref.ts";
import {
  AgentNotificationScheduler,
  countUnseenAgentStatuses,
  matchesAgentNotification,
  type AgentNotificationDelivery,
  type AgentNotificationKind,
} from "../lib/agentNotificationCore.ts";
import { claimAgentNotification } from "../lib/agentNotificationClaim.ts";

let audioContext: AudioContext | null = null;

function playCue(kind: AgentNotificationKind): void {
  try {
    const Context = window.AudioContext
      ?? (window as typeof window & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!Context) return;
    audioContext ??= new Context();
    const context = audioContext;
    if (context.state === "suspended") void context.resume().catch(() => {});
    const start = context.currentTime;
    const note = (frequency: number, offset: number, duration: number, peak: number) => {
      const oscillator = context.createOscillator();
      const gain = context.createGain();
      oscillator.type = "sine";
      oscillator.frequency.value = frequency;
      gain.gain.setValueAtTime(0.001, start + offset);
      gain.gain.linearRampToValueAtTime(peak, start + offset + 0.01);
      gain.gain.exponentialRampToValueAtTime(0.001, start + offset + duration);
      oscillator.connect(gain).connect(context.destination);
      oscillator.start(start + offset);
      oscillator.stop(start + offset + duration + 0.02);
    };
    if (kind === "blocked") {
      note(660, 0, 0.12, 0.12);
      note(880, 0.14, 0.15, 0.12);
    } else {
      note(523, 0, 0.15, 0.08);
    }
  } catch { /* audio is an optional delivery surface */ }
}


export function AgentNotificationBridge() {
  const location = useLocation();
  const navigate = useNavigate();
  const baseTitle = typeof document === "undefined"
    ? "Roost"
    : (document.title.replace(/^\(\d+\)\s+/, "") || "Roost");

  const viewSession = (sessionId: string): void => {
    const session = rootStore.sessions[sessionId];
    navigate(session ? terminalHref(session) : `/s/${sessionId}`);
  };

  const deliver = async (
    sessionId: string,
    pending: Pick<AgentNotificationDelivery, "revision" | "kind">,
  ): Promise<void> => {
    const status = rootStore.agent_status[sessionId] as AgentStatus | undefined;
    if (!matchesAgentNotification(status, pending)) return;
    const active = activeSessionForPath(location.pathname);
    if (isPageVisible() && active?.id === sessionId) {
      markAgentSeen(sessionId, status.revision);
      return;
    }
    if (seenAgentRevision(sessionId) >= pending.revision) return;

    const prefs = notifyPrefs();
    const soundEnabled = pending.kind === "blocked" ? prefs.blockedSound : prefs.doneSound;
    if (!prefs.inApp && !soundEnabled) return;
    if (!await claimAgentNotification(sessionId, pending.revision, pending.kind)) return;
    if (seenAgentRevision(sessionId) >= pending.revision) return;

    const session = rootStore.sessions[sessionId];
    const title = session ? sessionTitle(session) : "Terminal";
    if (prefs.inApp) {
      let dismiss = () => {};
      dismiss = addToast(
        pending.kind === "blocked" ? `${title} needs your input` : `${title} finished`,
        pending.kind === "blocked" ? "warn" : "ok",
        {
          details: status.message,
          ttlMs: pending.kind === "blocked" ? 8_000 : 5_000,
          action: {
            label: "View",
            onClick: () => {
              dismiss();
              viewSession(sessionId);
            },
          },
        },
      );
    }
    if (soundEnabled) playCue(pending.kind);
  };

  const scheduler = new AgentNotificationScheduler({
    statusFor: (sessionId) =>
      rootStore.agent_status[sessionId] as AgentStatus | undefined,
    isViewed: (sessionId) =>
      isPageVisible() && activeSessionForPath(location.pathname)?.id === sessionId,
    markSeen: (sessionId, revision) => { markAgentSeen(sessionId, revision); },
    deliver: (delivery) => { void deliver(delivery.sessionId, delivery); },
  });

  createEffect(() => {
    const path = location.pathname;
    if (!pageVisible()) return;
    const session = activeSessionForPath(path);
    if (!session) return;
    const status = rootStore.agent_status[session.id] as AgentStatus | undefined;
    if (!status) return;
    scheduler.view(session.id, status.revision);
  });

  createEffect(() => {
    const enabled = notifyPrefs().desktop;
    if (enabled) void ensurePushSubscription();
  });

  createEffect(() => {
    const count = countUnseenAgentStatuses(
      Object.values(rootStore.agent_status),
      seenAgentRevision,
    );
    const enabled = notifyPrefs().titleBadge;
    if (typeof document !== "undefined") {
      document.title = enabled && count > 0 ? `(${count}) ${baseTitle}` : baseTitle;
    }
  });

  onMount(() => {
    const stopSeenPersistence = startAgentSeenPersistence();
    const unsubscribeStatus = subscribeAgentStatus((change) => scheduler.handle(change));
    const onServiceWorkerMessage = (event: MessageEvent<unknown>) => {
      const value = event.data as { type?: unknown; sessionId?: unknown } | null;
      if (
        value?.type === "roost-navigate"
        && typeof value.sessionId === "string"
        && SessionId.safeParse(value.sessionId).success
      ) viewSession(value.sessionId);
    };
    navigator.serviceWorker?.addEventListener("message", onServiceWorkerMessage);
    onCleanup(() => {
      unsubscribeStatus();
      stopSeenPersistence();
      navigator.serviceWorker?.removeEventListener("message", onServiceWorkerMessage);
      scheduler.dispose();
      if (typeof document !== "undefined") document.title = baseTitle;
    });
  });

  return null;
}
