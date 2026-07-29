// Native light-DOM controls for the agent transcript.
//
// One proven reason, and it is a focus bug, not a styling one:
//
// The Settings md/* TextField renders a Material Web custom element whose real
// <textarea> lives in a SHADOW ROOT. CellTerminal installs a document-level
// mousedown guard that keeps terminal focus unless the click landed on a focus
// owner, matched with `target.closest('input, textarea, …')` — but a click
// inside a shadow root RETARGETS e.target to the host element
// (<md-outlined-text-field>), which matches none of those selectors. So the
// guard called preventDefault() and focus never reached the composer: no caret
// and keystrokes routed to the PTY on desktop, no keyboard at all on mobile.
// A native textarea in the light DOM is seen by that allowlist for free, and by
// every other focus heuristic (including the browser's own tap-to-keyboard).
//
// FOCUS_OWNERS in CellTerminal.tsx was widened too, so the guard can no longer
// eat focus from a Material text field anywhere in the app.
//
// No claim is made here about theming: every --md-* / --md-sys-color-* property
// is declared in styles/theme-vars.css and imported eagerly from main.tsx.

import { type Component, type JSX, splitProps } from "solid-js";

// 16px floor on the composer input: iOS Safari zooms the viewport when a
// focused field's font-size is below 16px, which yanks the whole transcript.
export const INPUT_FONT_SIZE = "16px";

type ButtonVariant = "filled" | "tonal" | "text";

const BUTTON_COLORS: Record<ButtonVariant, { bg: string; fg: string; border: string }> = {
  filled: { bg: "var(--md-primary)", fg: "var(--md-on-primary)", border: "transparent" },
  tonal: { bg: "var(--md-surface-container-high)", fg: "var(--md-on-surface)", border: "var(--md-outline-variant)" },
  text: { bg: "transparent", fg: "var(--md-on-surface-variant)", border: "transparent" },
};

/** A real <button>. Focusable, tappable, keyboard-activatable, no shadow DOM. */
export const AgentButton: Component<
  {
    variant?: ButtonVariant;
    children: JSX.Element;
  } & JSX.ButtonHTMLAttributes<HTMLButtonElement>
> = (props) => {
  const [own, rest] = splitProps(props, ["variant", "children", "style"]);
  const c = () => BUTTON_COLORS[own.variant ?? "tonal"];
  return (
    <button
      type="button"
      {...rest}
      style={{
        display: "inline-flex",
        "align-items": "center",
        "justify-content": "center",
        gap: "var(--md-space-2)",
        "min-height": "40px",
        padding: "0 var(--md-space-4)",
        "border-radius": "var(--md-shape-full)",
        border: `1px solid ${c().border}`,
        background: c().bg,
        color: c().fg,
        "font-family": "inherit",
        "font-size": "var(--md-label-l-size)",
        "font-weight": "var(--md-label-l-weight)",
        "line-height": "var(--md-label-l-line)",
        cursor: props.disabled ? "default" : "pointer",
        opacity: props.disabled ? "0.38" : "1",
        "white-space": "nowrap",
        transition: "background 120ms ease, opacity 120ms ease",
        ...(own.style as JSX.CSSProperties),
      }}
    >
      {own.children}
    </button>
  );
};

/** A real <textarea> that grows with its content, capped so the transcript
 *  always keeps room. `ref` is exposed so callers can focus it. */
export const AgentTextArea: Component<{
  value: string;
  onInput: (v: string) => void;
  placeholder?: string;
  ariaLabel: string;
  testId?: string;
  disabled?: boolean;
  maxRows?: number;
  onKeyDown?: (e: KeyboardEvent) => void;
  ref?: (el: HTMLTextAreaElement) => void;
}> = (props) => {
  let el: HTMLTextAreaElement | undefined;
  const LINE_HEIGHT = 22;
  const resize = (): void => {
    if (!el) return;
    el.style.height = "auto";
    const max = LINE_HEIGHT * (props.maxRows ?? 8);
    el.style.height = `${Math.min(el.scrollHeight, max)}px`;
    el.style["overflow-y" as never] = el.scrollHeight > max ? "auto" : "hidden";
  };
  return (
    <textarea
      ref={(node) => {
        el = node;
        props.ref?.(node);
        queueMicrotask(resize);
      }}
      value={props.value}
      rows={1}
      aria-label={props.ariaLabel}
      placeholder={props.placeholder}
      data-testid={props.testId}
      disabled={props.disabled}
      // Tells a touch keyboard to show a send affordance instead of a newline.
      enterkeyhint="send"
      autocapitalize="sentences"
      spellcheck={true}
      onInput={(e) => {
        props.onInput(e.currentTarget.value);
        resize();
      }}
      onKeyDown={props.onKeyDown}
      style={{
        flex: "1",
        "min-width": "0",
        resize: "none",
        overflow: "hidden",
        "max-height": `${LINE_HEIGHT * (props.maxRows ?? 8)}px`,
        padding: "10px var(--md-space-3)",
        "border-radius": "var(--md-shape-lg)",
        border: "1px solid var(--md-outline-variant)",
        background: "var(--md-surface-container-highest)",
        color: "var(--md-on-surface)",
        "font-family": "inherit",
        "font-size": INPUT_FONT_SIZE,
        "line-height": `${LINE_HEIGHT}px`,
        outline: "none",
      }}
      onFocus={(e) => {
        e.currentTarget.style.border = "1px solid var(--md-primary)";
      }}
      onBlur={(e) => {
        e.currentTarget.style.border = "1px solid var(--md-outline-variant)";
      }}
    />
  );
};
