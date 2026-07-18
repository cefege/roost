// TerminalNavButtons — a keyboard-free input pad for touch. A small FAB
// (keyboard icon, sibling to the mic) toggles a pop-up control pad: Esc on
// top, the ▲ / ◀▼▶ arrow cluster, Enter on the bottom. Tap the FAB to pop
// the pad open; tap again to collapse back to the FAB. Each key sends the
// real terminal byte sequence (Esc = 0x1b, arrows = CSI A/B/C/D, Enter = CR);
// nothing here parses the terminal. Open/closed state is module-level (shared
// across every deck-mounted pad) and persisted, so toggling one toggles all
// and the choice survives reload. Styling: styles/voice-input.css (.term-nav,
// .term-nav-toggle). Caller: CellTerminal.tsx (compact/touch/micOnDesktop).

import { createSignal, Show } from "solid-js";
import { inputChannel } from "../ws/input-channel.ts";
import { mouseForwardEnabled, toggleMouseForward } from "../lib/mouseForwardPref.ts";
import { onFabPointerDown } from "../lib/fabDragOffset.ts";

interface Props {
  sessionId: string;
}

const ESC = new Uint8Array([27]); // ESC
const UP = new Uint8Array([27, 91, 65]); // ESC [ A
const DOWN = new Uint8Array([27, 91, 66]); // ESC [ B
const RIGHT = new Uint8Array([27, 91, 67]); // ESC [ C
const LEFT = new Uint8Array([27, 91, 68]); // ESC [ D
const ENTER = new Uint8Array([13]); // CR
// PgUp / PgDn — scroll claude's alt-screen viewport (its pager reads these).
const PGUP = new Uint8Array([27, 91, 53, 126]); // ESC [ 5 ~
const PGDN = new Uint8Array([27, 91, 54, 126]); // ESC [ 6 ~

// Shared across all deck-mounted pads (the deck renders one Terminal —
// hence one TerminalNavButtons — per open session). A module-level signal
// keeps every instance in lock-step; localStorage makes the choice sticky.
const PAD_OPEN_KEY = "roostNavPadOpen";
const readPadOpen = (): boolean => {
  try { return localStorage.getItem(PAD_OPEN_KEY) === "1"; } catch { return false; }
};
const [padOpen, setPadOpen] = createSignal(readPadOpen());
const togglePad = () =>
  setPadOpen((open) => {
    const next = !open;
    try { localStorage.setItem(PAD_OPEN_KEY, next ? "1" : "0"); } catch { /* ignore */ }
    return next;
  });

export function TerminalNavButtons(props: Props) {
  const send = (bytes: Uint8Array) => inputChannel.sendInput(props.sessionId, bytes);

  return (
    <>
      <Show when={padOpen()}>
        <div class="term-nav" data-testid="terminal-nav-buttons">
          <NavKey area="esc" testid="nav-esc" label="esc" onClick={() => send(ESC)} />
          <NavKey area="pgup" testid="nav-pgup" icon="keyboard_double_arrow_up" onClick={() => send(PGUP)} />
          <NavKey area="up" testid="nav-up" icon="keyboard_arrow_up" onClick={() => send(UP)} />
          <NavKey area="pgdn" testid="nav-pgdn" icon="keyboard_double_arrow_down" onClick={() => send(PGDN)} />
          <NavKey area="left" testid="nav-left" icon="keyboard_arrow_left" onClick={() => send(LEFT)} />
          <NavKey area="down" testid="nav-down" icon="keyboard_arrow_down" onClick={() => send(DOWN)} />
          <NavKey area="right" testid="nav-right" icon="keyboard_arrow_right" onClick={() => send(RIGHT)} />
          <NavKey area="enter" testid="nav-enter" icon="keyboard_return" onClick={() => send(ENTER)} />
          {/* Mouse toggle: forward swipe/click to claude (scroll its fullscreen)
              vs native browser select/scroll. Highlighted when ON. */}
          <button
            type="button"
            class="term-nav__key term-nav__key--mouse"
            data-testid="nav-mouse"
            data-active={mouseForwardEnabled() ? "true" : "false"}
            aria-label="Toggle mouse forwarding"
            aria-pressed={mouseForwardEnabled()}
            onClick={toggleMouseForward}
          >
            <span class="term-nav__icon">mouse</span>
            <span class="term-nav__label">{mouseForwardEnabled() ? "on" : "off"}</span>
          </button>
        </div>
      </Show>
      <button
        type="button"
        class="term-nav-toggle"
        data-testid="terminal-nav-toggle"
        data-open={padOpen() ? "true" : "false"}
        aria-label={padOpen() ? "Hide keys" : "Show keys"}
        onPointerDown={onFabPointerDown}
        onClick={togglePad}
      >
        <span class="term-nav-toggle__icon">
          {padOpen() ? "keyboard_arrow_down" : "keyboard"}
        </span>
      </button>
    </>
  );
}

function NavKey(props: {
  area: "esc" | "up" | "left" | "down" | "right" | "enter" | "pgup" | "pgdn";
  testid: string;
  icon?: string;
  label?: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      class={`term-nav__key term-nav__key--${props.area}`}
      data-testid={props.testid}
      aria-label={props.testid}
      onClick={props.onClick}
    >
      {props.icon
        ? <span class="term-nav__icon">{props.icon}</span>
        : <span class="term-nav__label">{props.label}</span>}
    </button>
  );
}
