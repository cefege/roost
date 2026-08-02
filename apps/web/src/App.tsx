// Root component. Wraps Router with full route table per R0.18.
// Boots sync on mount (store/sync.ts). No SolidStart Router — plain @solidjs/router.
// AppErrorBoundary outermost; ConnectionBanner + WhatsNewDialog always mounted.

import { Router, Route, Navigate, useNavigate } from "@solidjs/router";
import { createMemo, Show, onMount, onCleanup, lazy } from "solid-js";
import type { JSX } from "solid-js";
import { ROUTES } from "./routes.ts";
import { AppShell } from "./components/layout/AppShell.tsx";
import { HomeLanding } from "./components/HomeLanding.tsx";
import { MainPane } from "./components/MainPane.tsx";
import { CommandPalette } from "./components/CommandPalette.tsx";
import { TransferStack } from "./components/TransferCard.tsx";
import { installKeyboardShortcuts, setSettingsOpener } from "./lib/keyboardShortcuts.ts";
import { rootStore } from "./store/root.ts";
import { bootstrapSync } from "./store/sync-bootstrap.ts";
import { AppErrorBoundary } from "./components/AppErrorBoundary.tsx";
import { ConnectionBanner } from "./components/ConnectionBanner.tsx";
import { VersionBanner } from "./components/VersionBanner.tsx";
import { WhatsNewDialog } from "./components/WhatsNewDialog.tsx";
import { QueueTaskDialog } from "./components/QueueTaskDialog.tsx";
import { ToastContainer } from "./components/ToastContainer.tsx";
import { PairRequestNotifier } from "./components/PairRequestNotifier.tsx";
import { UndoCloseBanner } from "./components/UndoCloseBanner.tsx";
import { TransferConsoleHost } from "./components/TransferConsoleHost.tsx";
import { TransferDialogHost } from "./components/TransferDialog.tsx";
import { RenameDialogHost } from "./components/RenameDialog.tsx";
import { getLastTerminalPath } from "./lib/lastVisited.ts";
import { shouldBootRestore, consumeBootRestore } from "./lib/bootRestore.ts";
import { UiBridge } from "./lib/uiBridge.tsx";

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
  // (+ its store/connect deps) is code-split into its own chunk fetched
  // ONLY when localStorage.roostSmoke==="1" — non-smoke users never pay
  // for it. Matches the lazy() split policy at L34. The harness reloads
  // with the flag set and injects after load, so the async gap is
  // invisible to run.js step1's window.__smoke check.
  if (typeof localStorage !== "undefined" && localStorage.getItem("roostSmoke") === "1") {
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
  // RootShell is the always-mounted overlay/portal layer that needs router
  // primitives (useNavigate). Solid's <Router> requires `useNavigate` etc.
  // to be called INSIDE the router subtree — that's why CommandPalette,
  // HelpOverlay, etc. live here, wrapped by Router's root path.
  // Solid Router v0.16 passes RouteSectionProps; `children` is optional
  // there but RootShell always renders it as the slot. Accept the wider
  // type + fall back to `<></>` when missing.
  function SmokeRouterBridge() {
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
      setSettingsOpener(() => navigate("/settings/machines"));
      onCleanup(() => setSettingsOpener(null));
    });
    return null;
  }

  function RootShell(props: { children?: JSX.Element }) {
    return (
      <>
        {props.children}
        <SmokeRouterBridge />
        <ShortcutRouterBridge />
        <UiBridge />
        <CommandPalette />
        <HelpOverlay />
        <WhatsNewDialog />
        <QueueTaskDialog />
        <ToastContainer />
        <PairRequestNotifier />
        <UndoCloseBanner />
        <TransferDialogHost />
        <TransferConsoleHost />
        <RenameDialogHost />
        <TransferStack />
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
        <Route path={ROUTES.SETTINGS} component={SettingsRoot} />
        <Route path={ROUTES.PAIR} component={() => <Onboarding />} />
        <Route path={ROUTES.HELP} component={Help} />
        <Route path={ROUTES.DESIGN} component={DesignGallery} />
        <Route path="*" component={() => <Navigate href="/" />} />
      </Router>
    </AppErrorBoundary>
  );
}
