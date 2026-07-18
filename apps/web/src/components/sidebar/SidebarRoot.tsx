// Sidebar root: SAME nav on every route (Author 2026-06-12 "needs to be
// the same same fucking sidebar"). claude.ai/code-style server →
// workspace → pane tree, rendered identically whether the user is at /,
// /w/<id>, /swarm/t/<fp>/<ch>, /inbox, /queue, /search, or /file/*.
// Old per-route view switching killed orientation when entering a
// terminal. Drop the Switch — always render AllView.

import { AllView } from "./AllView.tsx";

export function SidebarRoot() {
  return <AllView />;
}
