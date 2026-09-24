// One terminal card in the mobile terminal grid. Chrome's
// tab_grid_card_item_layout geometry: a 40px header (favicon + title +
// truncated subtitle + close ✕) over a terminal preview area with asymmetric
// corners (12px top / 20px bottom). Swiping past CARD_DISMISS_PX, or a fast
// flick, closes the terminal. Rendered by WorkspaceTabsSheet in MobileDeckBar.
import { Show, createMemo, createSignal, onMount } from "solid-js";
import type { Session } from "@roost/protocol/wire";
import { sessionTitle, programSubtitle } from "../../lib/sessionTitle.ts";
import { renderPreview } from "../../renderer/terminalPreview.ts";
import { createTrackedTimeouts } from "../trackedTimeout.ts";
import { shouldDismissCard, cardSwipeAlpha, CARD_DISMISS_PX } from "../../lib/deckSwipe.ts";
import { AgentStatusIndicator } from "../agents/AgentStatusIndicator.tsx";

export function TerminalCard(props: {
  session: Session;
  active: boolean;
  onSelect: (id: string) => void;
  onClose: (s: Session) => void;
  onCloseSheet: () => void;
  selectionMode: boolean;
  selected: boolean;
  onToggleSelect: (id: string) => void;
}) {
  const s = () => props.session;
  const name = createMemo(() => sessionTitle(s()));
  const sub = createMemo(() => programSubtitle(s()));
  const branch = () => s().git_branch ?? null;
  // Compact header subtitle: branch if available, else cloud subtitle.
  const subtitle = () => branch() ?? sub();

  // ── Terminal preview render ───────────────────────────────────────────
  let previewRef: HTMLDivElement | undefined;
  const [hasPreview, setHasPreview] = createSignal(false);
  onMount(() => {
    if (previewRef) setHasPreview(renderPreview(s().id, previewRef));
  });
  const setTimeoutTracked = createTrackedTimeouts();

  // ── Swipe-to-close (touch only) ───────────────────────────────────────
  // Chrome TabGridItemTouchHelperCallback: drag a card left/right and it slides
  // straight (translateX, NO rotation) while fading toward transparent; past a
  // fixed 144px of travel it dismisses, or a fast directional flick dismisses
  // below that. Below threshold → spring back. Vertical → let the grid scroll.
  const [swipeX, setSwipeX] = createSignal(0);
  const [swiping, setSwiping] = createSignal(false);
  let _touchStartX = 0;
  let _touchStartY = 0;
  let _swipeAxis: "none" | "x" | "y" = "none";
  let _swiped = false;
  let _lastX = 0;
  let _lastT = 0;
  let _vx = 0;
  let _overThreshold = false;

  function onTouchStart(e: TouchEvent) {
    if (props.selectionMode) return;
    const t = e.touches[0];
    if (!t) return;
    _touchStartX = t.clientX;
    _touchStartY = t.clientY;
    _lastX = t.clientX;
    _lastT = e.timeStamp;
    _vx = 0;
    _swipeAxis = "none";
    _swiped = false;
    _overThreshold = false;
    setSwiping(true);
  }
  function onTouchMove(e: TouchEvent) {
    const t = e.touches[0];
    if (!t) return;
    const dx = t.clientX - _touchStartX;
    const dy = t.clientY - _touchStartY;
    if (_swipeAxis === "none") {
      if (Math.abs(dx) < 10 && Math.abs(dy) < 10) return;
      _swipeAxis = Math.abs(dx) > Math.abs(dy) * 1.5 ? "x" : "y";
    }
    if (_swipeAxis !== "x") return; // vertical → let the grid scroll
    e.preventDefault();
    setSwipeX(dx);
    if (Math.abs(dx) > 24) _swiped = true;
    const dt = e.timeStamp - _lastT;
    if (dt > 0) _vx = (t.clientX - _lastX) / dt;
    _lastX = t.clientX;
    _lastT = e.timeStamp;
    const over = Math.abs(dx) >= CARD_DISMISS_PX;
    if (over && !_overThreshold) navigator.vibrate?.(8); // Chrome: haptic once on cross-up
    _overThreshold = over;
  }
  function onTouchEnd() {
    setSwiping(false);
    if (shouldDismissCard(swipeX(), _vx)) {
      _swiped = true; // suppress the trailing click during slide-off
      const off = swipeX() > 0 ? window.innerWidth : -window.innerWidth;
      setSwipeX(off);
      setTimeoutTracked(() => props.onClose(s()), 180);
    } else {
      setSwipeX(0);
      _swiped = false; // short drag that didn't dismiss → don't block next tap
    }
  }

  function activate() {
    if (props.selectionMode) { props.onToggleSelect(s().id); return; }
    if (_swiped) { _swiped = false; return; }
    props.onSelect(s().id);
    props.onCloseSheet();
  }

  return (
    <div
      class="terminal-card"
      data-testid={`terminal-card-${s().id}`}
      data-active={props.active ? "true" : "false"}
      data-selected={props.selected ? "true" : "false"}
      role="button"
      tabindex="0"
      title={name()}
      style={{
        transform: `translateX(${swipeX()}px)`,
        opacity: String(cardSwipeAlpha(swipeX())),
        transition: swiping()
          ? "none"
          : "transform var(--md-sys-motion-duration-short4, 200ms) var(--md-sys-motion-easing-emphasized-decelerate, cubic-bezier(0.05, 0.7, 0.1, 1)), opacity var(--md-sys-motion-duration-short4, 200ms) var(--md-sys-motion-easing-emphasized-decelerate, cubic-bezier(0.05, 0.7, 0.1, 1))",
      }}
      onClick={activate}
      onTouchStart={onTouchStart}
      onTouchMove={onTouchMove}
      onTouchEnd={onTouchEnd}
      onKeyDown={(e: KeyboardEvent) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          activate();
        }
      }}
    >
      {/* Header — favicon + title + compact subtitle. Close ✕ overlaps top-right. */}
      <div class="terminal-card-header">
        <span class="terminal-card-favicon">
          <span class="terminal-card-glyph">$</span>
        </span>
        <div class="terminal-card-title-wrap">
          <span class="terminal-card-title">{name()}</span>
          <Show when={subtitle()}>
            <span class="terminal-card-subtitle">{subtitle()}</span>
          </Show>
          <AgentStatusIndicator sessionId={s().id} />
        </div>
      </div>

      {/* Close ✕ — 48px touch target, 18px visible icon, top-right corner. */}
      <Show when={!props.selectionMode}>
        <button
          type="button"
          class="terminal-card-close"
          data-testid={`terminal-card-close-${s().id}`}
          aria-label="Close terminal session"
          onClick={(e: MouseEvent) => {
            e.stopPropagation();
            e.preventDefault();
            props.onClose(s());
          }}
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
            <path d="M18 6 6 18M6 6l12 12" />
          </svg>
        </button>
      </Show>

      {/* Selection check — filled tint when selected, top-right corner. */}
      <Show when={props.selectionMode}>
        <span
          class="terminal-card-check"
          data-testid={`terminal-card-check-${s().id}`}
          data-checked={props.selected ? "true" : "false"}
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M20 6 9 17l-5-5" />
          </svg>
        </span>
      </Show>

      {/* Preview area — real terminal text or faux glyph fallback.
          Asymmetric corners (12/20px). Fixed 160px height = uniform cards. */}
      <div class="terminal-card-preview">
        <div
          ref={previewRef}
          class="terminal-card-preview-text"
          style={{ display: hasPreview() ? "block" : "none" }}
        />
        <Show when={!hasPreview()}>
          <span class="terminal-card-preview-glyph" style={{ color: "var(--text-lo)" }}>
            <span class="terminal-card-glyph">$</span>
          </span>
        </Show>
      </div>
    </div>
  );
}
