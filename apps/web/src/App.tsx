// Root component. Owns the route table and the router-scoped access gate: the
// protected routes and overlay shell mount only once this browser is authorized.
// Boots sync on mount (store/sync.ts). No SolidStart Router — plain @solidjs/router.
// AppErrorBoundary is outermost; the connection banner stays outside the gate.

import { Router, Route, Navigate, useNavigate } from "@solidjs/router";
import { createMemo, Show, Switch, Match, onMount, onCleanup, lazy } from "solid-js";
import type { JSX } from "solid-js";
import { ROUTES, settingsPaneHref } from "./routes.ts";
import { AppShell } from "./components/layout/AppShell.tsx";
import { HomeLanding } from "./components/HomeLanding.tsx";
import { MainPane } from "./components/MainPane.tsx";
import { CommandPalette } from "./components/palette/CommandPalette.tsx";
import { installKeyboardShortcuts, setSettingsOpener } from "./lib/keyboardShortcuts.ts";
import { installSpatialNavigation } from "./lib/spatialNavigation.ts";
import { installGamepadSource } from "./browser/gamepadSource.ts";
import { runPadActions } from "./lib/padActions.ts";
import { rootStore } from "./store/root.ts";
import { bootstrapSync } from "./store/sync-bootstrap.ts";
import { AppErrorBoundary } from "./components/AppErrorBoundary.tsx";
import { ConnectionBanner } from "./components/notifications/ConnectionBanner.tsx";
import { VersionBanner } from "./components/notifications/VersionBanner.tsx";
import { WhatsNewDialog } from "./components/notifications/WhatsNewDialog.tsx";
import { QueueTaskDialog } from "./components/agents/QueueTaskDialog.tsx";
import { NotificationDock } from "./components/notifications/NotificationDock.tsx";
import { RenameDialogHost } from "./components/RenameDialog.tsx";
import { getLastTerminalPath } from "./lib/lastVisited.ts";
import { shouldBootRestore, consumeBootRestore } from "./browser/bootRestore.ts";
import { UiBridge } from "./components/UiBridge.tsx";
import { AgentNotificationBridge } from "./components/notifications/AgentNotificationBridge.tsx";
import { PairApprovalProvider } from "./components/pairing/PairApprovalProvider.tsx";
import { PairingRequesterProvider } from "./components/pairing/PairingRequesterProvider.tsx";
import { AccessCheckingScreen } from "./components/pairing/AccessCheckingScreen.tsx";

// Code-split boundaries (ts-no-dynamic-import exception): solid `lazy` is the
// bundler's split mechanism — routes/overlays below load their chunk on first
// visit/open instead of riding the eager entry chunk (perf sweep C2.1). Solid
// lazy renders nothing until resolved; a one-frame blank on first open is the
// accepted trade (no Suspense wrapper needed).
const Onboarding = lazy(() => import("./components/pairing/Onboarding.tsx").then((m) => ({ default: m.Onboarding })));
const SettingsRoot = lazy(() => import("./components/Settings/SettingsRoot.tsx").then((m) => ({ default: m.SettingsRoot })));
const Help = lazy(() => import("./components/palette/Help.tsx").then((m) => ({ default: m.Help })));
const DesignGallery = lazy(() => import("./components/design/DesignGallery.tsx").then((m) => ({ default: m.DesignGallery })));
const HelpOverlay = lazy(() => import("./components/palette/HelpOverlay.tsx").then((m) => ({ default: m.HelpOverlay })));
const ControllerMap = lazy(() => import("./components/palette/ControllerMap.tsx").then((m) => ({ default: m.ControllerMap })));
const BrowsePage = lazy(() => import("./components/browse/BrowsePage.tsx").then((m) => ({ default: m.BrowsePage })));
const BrowseRedirect = lazy(() => import("./components/browse/BrowsePage.tsx").then((m) => ({ default: m.BrowseRedirect })));

function WorkspaceRedirect() {
  // Boot restore (Author 2026-07-06, reverses the 2026-06-23 hello-page default):
  // land straight back in the terminal you were last on. Stored browser-local
  // as the stable /t/ href, so a respawn in the same folder still resolves; a
  // truly-dead one is caught by MainPane's safety net → back here → HomeLanding.
  const hasWorkers = createMemo(() => Object.keys(rootStore.workers).length > 0);
  if (shouldBootRestore()) {
    consumeBootRestore();
    const last = getLastTerminalPath();
    if (last) return <Navigate href={last} />;
  }
  return <Show when={hasWorkers()} fallback={<Onboarding />}><HomeLanding /></Show>;
}

export function App() {
  bootstrapSync();
  // Smoke backdoor: dynamic-import so the ~286-line window.__smoke API
  // (+ its store/connect deps) is code-split into its own chunk that exists
  // ONLY in builds made with VITE_ROOST_SMOKE=1 (the test tier) AND when
  // localStorage.roostSmoke==="1" at runtime. Production builds fold this
  // branch out entirely — the harness chunk is absent from dist, so the
  // backdoor cannot be armed by flipping a localStorage key. The test
  // reloads with the flag set and injects after load, so the async gap is
  // invisible to run.js step1's window.__smoke check.
  if (import.meta.env.VITE_ROOST_SMOKE === "1"
    && typeof localStorage !== "undefined" && localStorage.getItem("roostSmoke") === "1") {
    void import("./smoke/smoke.ts").then((m) => m.maybeInstallSmokeBackdoor());
  }
  onMount(() => {
    const cleanup = installKeyboardShortcuts();
    onCleanup(cleanup);
    const cleanupSpatial = installSpatialNavigation();
    onCleanup(cleanupSpatial);
    const cleanupPad = installGamepadSource(runPadActions);
    onCleanup(cleanupPad);
    // Author 2026-06-13: right-click should be roost-custom everywhere.
    // Suppress the browser's native context menu app-wide; per-component
    // onContextMenu handlers (e.g. WorkspaceRow → workspace actions)
    // call e.preventDefault themselves to surface their custom menu.
    // Carve out form inputs + textareas + .wterm so users can still
    // paste, copy, spell-check, etc. in actual text fields.
    const suppress = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null;
      if (!target) return;
      if (target.closest("input, textarea, .wterm, [contenteditable=true]")) return;
      e.preventDefault();
    };
    document.addEventListener("contextmenu", suppress);
    onCleanup(() => document.removeEventListener("contextmenu", suppress));
  });
  // RootShell owns the router-scoped access gate and protected overlay/portal
  // layer. Solid's <Router> requires `useNavigate` etc. to be called INSIDE
  // the router subtree, so these hosts cannot move above it.
  // Solid Router v0.16 passes RouteSectionProps; `children` is optional
  // there but RootShell always renders it as the slot.
  function SmokeRouterBridge() {
    if (import.meta.env.VITE_ROOST_SMOKE !== "1") return null;
    const navigate = useNavigate();
    onMount(() => {
      let enabled = false;
      try { enabled = localStorage.getItem("roostSmoke") === "1"; } catch { /* unavailable document */ }
      if (!enabled) return;
      const onNavigate = (event: Event) => {
        const href = (event as CustomEvent<string>).detail;
        if (typeof href === "string") navigate(href);
      };
      window.addEventListener("roost-smoke-navigate", onNavigate);
      onCleanup(() => window.removeEventListener("roost-smoke-navigate", onNavigate));
    });
    return null;
  }

  // ⌘, needs a router-scoped navigate, and keyboardShortcuts.ts is a leaf with
  // no router access — so hand it one from inside <Router>.
  function ShortcutRouterBridge() {
    const navigate = useNavigate();
    onMount(() => {
      setSettingsOpener(() => navigate(settingsPaneHref("machines")));
      onCleanup(() => setSettingsOpener(null));
    });
    return null;
  }

  function RootShell(props: { children?: JSX.Element }) {
    // Overlays reach the coordinator the moment they mount, so they wait for
    // identity discovery to settle rather than racing the first RPC.
    const coordinatorDiscovered = createMemo(() => rootStore.coord_identity !== null);
    const accessState = () => rootStore.browser_access_state;

    // Both providers sit above the gate so an access transition never remounts
    // them: the requester ceremony must survive checking → unauthorized →
    // authorized, and the approver code must survive route changes.
    return (
      <PairingRequesterProvider>
        <PairApprovalProvider enabled={accessState() === "authorized"}>
          <SmokeRouterBridge />
          <Switch>
            <Match when={accessState() === "checking"}>
              <AccessCheckingScreen />
            </Match>
            <Match when={accessState() === "unauthorized"}>
              <Onboarding />
            </Match>
            <Match when={accessState() === "authorized"}>
              <VersionBanner />
              {props.children}
              <Show when={coordinatorDiscovered()}>
                <ShortcutRouterBridge />
                <UiBridge />
                <AgentNotificationBridge />
                <CommandPalette />
                <HelpOverlay />
                <ControllerMap />
                <WhatsNewDialog />
                <QueueTaskDialog />
                <NotificationDock />
                <RenameDialogHost />
              </Show>
            </Match>
          </Switch>
        </PairApprovalProvider>
      </PairingRequesterProvider>
    );
  }

  return (
    <AppErrorBoundary>
      <ConnectionBanner />
      <Router root={RootShell}>
        <Route path="/" component={AppShell}>
          {/* Index "/" → HomeLanding INSIDE AppShell so the sidebar (desktop)
              / drawer + ☰ (mobile) are always present on the home page. */}
          <Route path={ROUTES.ROOT} component={WorkspaceRedirect} />
          {/* ONE route definition for every MainPane screen. Separate
              <Route> entries remount MainPane (and the terminal deck under
              it) on every /s ↔ /file ↔ /search crossing — Solid router keys
              the component instance to the route DEFINITION, not the
              component reference. A path array is one definition: switching
              between these URLs keeps MainPane mounted and the deck host
              (MainPane.tsx) just flips visibility. */}
          <Route
            path={[
              ROUTES.SESSION,
              ROUTES.TERMINAL_BY_FOLDER,
              ROUTES.WORKSPACE,
              ROUTES.WORKSPACE_TERMINAL,
              ROUTES.FILE,
              ROUTES.SEARCH,
            ]}
            component={MainPane}
          />
          <Route path={ROUTES.BROWSE_ROOT} component={BrowseRedirect} />
          <Route path={ROUTES.BROWSE} component={BrowsePage} />
          <Route path={ROUTES.SETTINGS} component={SettingsRoot} />
          <Route path={ROUTES.HELP} component={Help} />
        </Route>
        <Route path={ROUTES.PAIR} component={() => <Onboarding />} />
        <Route path={ROUTES.DESIGN} component={DesignGallery} />
        <Route path="*" component={() => <Navigate href="/" />} />
      </Router>
    </AppErrorBoundary>
  );
}
