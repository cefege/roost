import { type JSX, type Component, Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import "@material/web/dialog/dialog.js";

// ─── Dialog → real md-dialog (scrim + enter/exit motion + focus-trap + ESC) ──
// Replaces hand-rolled position:fixed backdrop modals that snapped in with no
// motion. `open` controls visibility; md-dialog fires `closed` on ESC / scrim /
// action — onClose should set the caller's open signal false.
export const Dialog: Component<{
  open: boolean;
  onClose: () => void;
  headline?: string;
  children: JSX.Element;
  actions?: JSX.Element;
}> = (props) => (
  <Dynamic component="md-dialog" prop:open={props.open} on:closed={() => props.onClose()}>
    <Show when={props.headline}>
      <div slot="headline">{props.headline}</div>
    </Show>
    <div slot="content">{props.children}</div>
    <Show when={props.actions}>
      <div slot="actions">{props.actions}</div>
    </Show>
  </Dynamic>
);
