import { type JSX, type Component, Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import "@material/web/dialog/dialog.js";

// ─── Dialog → real md-dialog (scrim + enter motion + focus-trap + ESC) ──────
// Replaces hand-rolled position:fixed backdrop modals that snapped in with no
// motion. `open` controls visibility; md-dialog fires `closed` on ESC / scrim /
// action — onClose should set the caller's open signal false.
//
// A showModal() <dialog> makes the whole rest of the document inert, and
// md-dialog keeps its native dialog open for the entire ~155ms exit animation.
// focus() elsewhere is silently dropped for that window, so a ⌘K palette opened
// right after a dialog closed accepted no keystrokes. `quick` skips the exit, so
// the top layer is released in the same task the close starts. Releasing it at
// the START of an animated exit is not possible here: md-dialog's own
// `dialog[open]{display:flex}` stops rendering the container the instant the
// native dialog closes, leaving only the scrim to animate — the lingering-scrim
// artifact overlayMotion.ts already rejected, where close is likewise instant.
export const Dialog: Component<{
  open: boolean;
  onClose: () => void;
  headline?: string;
  children: JSX.Element;
  actions?: JSX.Element;
}> = (props) => (
  <Dynamic
    component="md-dialog"
    prop:quick={!props.open}
    prop:open={props.open}
    on:close={skipExitAnimation}
    on:closed={() => props.onClose()}
  >
    <Show when={props.headline}>
      <div slot="headline">{props.headline}</div>
    </Show>
    <div slot="content">{props.children}</div>
    <Show when={props.actions}>
      <div slot="actions">{props.actions}</div>
    </Show>
  </Dynamic>
);

/** ESC and scrim clicks are closes md-dialog starts itself, with `open` still
 *  true — so the bound `quick` above cannot cover them. md-dialog dispatches
 *  `close` synchronously at the head of every exit, which is still in time for
 *  the animation to read the flag. */
function skipExitAnimation(event: Event): void {
  const host = event.currentTarget;
  if (host !== null && typeof host === "object" && "quick" in host) host.quick = true;
}
