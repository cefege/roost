// Binds typed UI state reporting and Sync-delivered commands to router context.
// RootShell mounts this once inside Router so commands share user navigation.
// The reporter receives the live pathname; the dispatcher receives both route
// access and navigation. This component renders nothing.

import { createEffect, on, onCleanup, onMount } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";
import {
  authoritativeUiReportSessionId,
  initUiStateReport,
  scheduleUiStateReport,
  scheduleUiStateReportOnSessionResolution,
} from "../lib/uiStateReport.ts";
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
  // Hydration can resolve an unchanged cold /s/:id after the initial report.
  createEffect(on(
    () => authoritativeUiReportSessionId(location.pathname),
    (currentSessionId, previousSessionId) => {
      scheduleUiStateReportOnSessionResolution(currentSessionId, previousSessionId);
    },
    { defer: true },
  ));
  return null;
}
