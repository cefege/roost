// Full-viewport "Connecting to Roost…" surface the access gate shows while this
// browser's authority is still unknown (rootStore.browser_access_state is
// "checking"). Mounted only by App.tsx's RootShell, in place of every protected
// surface, so no workbench chrome paints before authorization is known.
// Depends on md primitives and the .access-checking rules in workbench-shell.css.

import { StatusDot, Surface } from "../Settings/md/primitives.tsx";

export function AccessCheckingScreen() {
  return (
    <div class="access-checking" data-testid="access-checking">
      <Surface
        level={1}
        radius="lg"
        pad={6}
        border
        role="status"
        aria-live="polite"
        class="access-checking__card"
      >
        <StatusDot status="running" />
        <span class="md-body-l">Connecting to Roost…</span>
      </Surface>
    </div>
  );
}
