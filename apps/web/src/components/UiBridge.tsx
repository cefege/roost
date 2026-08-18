// ui-cc bridge — the ONE component that ties the agent UI channel to router
// context. Mounted once in App's RootShell (inside <Router>, always alive):
//   • starts the state reporter (lib/uiStateReport.ts) with a live pathname
//     accessor + re-schedules a report on every location change;
//   • registers the ui_command frame handler (lib/uiCommandDispatch.ts) with
//     useNavigate bound, so agent commands navigate like user clicks.
// Renders nothing. onCleanup reads no props (repo L11).

import { createEffect, on, onCleanup, onMount } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";
import { initUiStateReport, scheduleUiStateReport } from "../lib/uiStateReport.ts";
import { registerUiCommandHandler, handleUiCommand } from "../lib/uiCommandDispatch.ts";

export function UiBridge() {
  const navigate = useNavigate();
  const location = useLocation();
  const io = {
    navigate: (href: string) => navigate(href),
    getPath: () => location.pathname,
  };
  onMount(() => {
    const disposeReport = initUiStateReport(io.getPath);
    const unregister = registerUiCommandHandler((frame) => handleUiCommand(frame, io));
    onCleanup(() => { disposeReport(); unregister(); });
  });
  // Route change = navigation state change → report (debounced in the reporter).
  createEffect(on(() => location.pathname, () => scheduleUiStateReport(), { defer: true }));
  return null;
}
