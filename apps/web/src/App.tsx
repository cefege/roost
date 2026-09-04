// Root component. Owns the route table and router-scoped protected overlay shell.
// Boots sync on mount (store/sync.ts). No SolidStart Router — plain @solidjs/router.
// AppErrorBoundary is outermost; connection and version banners stay route-independent.

import { Router, Route, Navigate, useLocation, useNavigate } from "@solidjs/router";
import { createMemo, Show, onMount, onCleanup, lazy } from "solid-js";
import type { JSX } from "solid-js";
import { ROUTES, settingsPaneHref } from "./routes.ts";
import { AppShell } from "./components/layout/AppShell.tsx";
import { HomeLanding } from "./components/HomeLanding.tsx";
import { MainPane } from "./components/MainPane.tsx";
import { CommandPalette } from "./components/CommandPalette.tsx";
import { TransferStack } from "./components/TransferCard.tsx";
import { installKeyboardShortcuts, setSettingsOpener } from "./lib/keyboardShortcuts.ts";
import { hasConfirmedDashboardAccess, rootStore } from "./store/root.ts";
import { bootstrapSync } from "./store/sync-bootstrap.ts";
import { AppErrorBoundary } from "./components/AppErrorBoundary.tsx";
import { ManagedRouteGate } from "./components/ManagedRouteGate.tsx";
import { ConnectionBanner } from "./components/ConnectionBanner.tsx";
import { VersionBanner } from "./components/VersionBanner.tsx";
import { WhatsNewDialog } from "./components/WhatsNewDialog.tsx";
import { QueueTaskDialog } from "./components/QueueTaskDialog.tsx";
import { ToastContainer } from "./components/ToastContainer.tsx";
import { PairRequestNotifier } from "./components/PairRequestNotifier.tsx";
import { UndoCloseBanner } from "./components/UndoCloseBanner.tsx";
import { TransferDialogHost } from "./components/TransferDialog.tsx";
import { RenameDialogHost } from "./components/RenameDialog.tsx";
import { getLastTerminalPath } from "./lib/lastVisited.ts";
import { shouldBootRestore, consumeBootRestore } from "./lib/bootRestore.ts";
import { UiBridge } from "./components/UiBridge.tsx";
import { AgentNotificationBridge } from "./components/AgentNotificationBridge.tsx";
import { isManagedPublicRoute } from "./auth/managed-routes.ts";

// Code-split boundaries (ts-no-dynamic-import exception): solid `lazy` is the
// bundler's split mechanism — routes/overlays below load their chunk on first
// visit/open instead of riding the eager entry chunk (perf sweep C2.1). Solid
// lazy renders nothing until resolved; a one-frame blank on first open is the
// accepted trade (no Suspense wrapper needed).
const Onboarding = lazy(() => import("./components/Onboarding.tsx").then((m) => ({ default: m.Onboarding })));
const SettingsRoot = lazy(() => import("./components/Settings/SettingsRoot.tsx").then((m) => ({ default: m.SettingsRoot })));
const Help = lazy(() => import("./components/Help.tsx").then((m) => ({ default: m.Help })));
const DesignGallery = lazy(() => import("./components/DesignGallery.tsx").then((m) => ({ default: m.DesignGallery })));
const HelpOverlay = lazy(() => import("./components/HelpOverlay.tsx").then((m) => ({ default: m.HelpOverlay })));
const BrowsePage = lazy(() => import("./components/BrowsePage.tsx").then((m) => ({ default: m.BrowsePage })));
const BrowseRedirect = lazy(() => import("./components/BrowsePage.tsx").then((m) => ({ default: m.BrowseRedirect })));
const ManagedLogin = lazy(() => import("./components/ManagedLogin.tsx").then((m) => ({ default: m.ManagedLogin })));
const ManagedSignup = lazy(() => import("./components/ManagedSignup.tsx").then((m) => ({ default: m.ManagedSignup })));
const ManagedSignupVerify = lazy(() => import("./components/ManagedSignupVerify.tsx").then((m) => ({ default: m.ManagedSignupVerify })));
const ManagedGoogleComplete = lazy(() => import("./components/ManagedGoogleComplete.tsx").then((m) => ({ default: m.ManagedGoogleComplete })));
const ManagedOwnerActivation = lazy(() => import("./components/ManagedOwnerActivation.tsx").then((m) => ({ default: m.ManagedOwnerActivation })));
const ManagedForgotPassword = lazy(() => import("./components/ManagedForgotPassword.tsx").then((m) => ({ default: m.ManagedForgotPassword })));
const ManagedPasswordReset = lazy(() => import("./components/ManagedPasswordReset.tsx").then((m) => ({ default: m.ManagedPasswordReset })));

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
    void import("./lib/smoke.ts").then((m) => m.maybeInstallSmokeBackdoor());
  }
  onMount(() => {
    const cleanup = installKeyboardShortcuts();
    onCleanup(cleanup);
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
  // RootShell owns the router-scoped route gate and protected overlay/portal
  // layer. Solid's <Router> requires `useNavigate` etc. to be called INSIDE
  // the router subtree, so these hosts cannot move above it.
  // Solid Router v0.16 passes RouteSectionProps; `children` is optional
  // there but RootShell always renders it as the slot. Accept the wider
  // type + fall back to `<></>` when missing.
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
    const location = useLocation();
    const hasProtectedOverlayAccess = createMemo(() => {
      const saasMode = rootStore.coord_identity?.saas_mode;
      if (saasMode === undefined || isManagedPublicRoute(location.pathname)) return false;
      return !saasMode
        || (!rootStore.browser_unauthorized && hasConfirmedDashboardAccess());
    });

    return (
      <>
        <SmokeRouterBridge />
        <ManagedRouteGate>
          {props.children}
          <Show when={hasProtectedOverlayAccess()}>
            <ShortcutRouterBridge />
            <UiBridge />
            <AgentNotificationBridge />
            <CommandPalette />
            <HelpOverlay />
            <WhatsNewDialog />
            <QueueTaskDialog />
            <ToastContainer />
            <PairRequestNotifier />
            <UndoCloseBanner />
            <TransferDialogHost />
            <RenameDialogHost />
            <TransferStack />
          </Show>
        </ManagedRouteGate>
      </>
    );
  }

  return (
    <AppErrorBoundary>
      <ConnectionBanner />
      <VersionBanner />
      <Router root={RootShell}>
        <Route path="/" component={AppShell}>
          {/* Index "/" → HomeLanding INSIDE AppShell so the sidebar (desktop)
              / drawer + ☰ (mobile) are always present on the home page. */}
          <Route path={ROUTES.ROOT} component={WorkspaceRedirect} />
          <Route path={ROUTES.APP} component={HomeLanding} />
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
        </Route>
        <Route path={ROUTES.LOGIN} component={ManagedLogin} />
        <Route path={ROUTES.SIGNUP} component={ManagedSignup} />
        <Route path={ROUTES.SIGNUP_VERIFY} component={ManagedSignupVerify} />
        <Route path={ROUTES.GOOGLE_COMPLETE} component={ManagedGoogleComplete} />
        <Route path={ROUTES.ACTIVATE} component={ManagedOwnerActivation} />
        <Route path={ROUTES.FORGOT_PASSWORD} component={ManagedForgotPassword} />
        <Route path={ROUTES.RESET_PASSWORD} component={ManagedPasswordReset} />
        <Route path={ROUTES.SETTINGS} component={SettingsRoot} />
        <Route path={ROUTES.PAIR} component={() => <Onboarding />} />
        <Route path={ROUTES.HELP} component={Help} />
        <Route path={ROUTES.DESIGN} component={DesignGallery} />
        <Route path="*" component={() => <Navigate href="/" />} />
      </Router>
    </AppErrorBoundary>
  );
}
