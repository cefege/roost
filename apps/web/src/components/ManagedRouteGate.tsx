// Managed routes must wait for coordinator-backed access before protected UI renders.
// The root router wraps managed pages with this gate and redirects public or denied states.
// Deployment mode, route classification, and dashboard bootstrap remain the authority.

import { Navigate, useLocation } from "@solidjs/router";
import { createEffect, createMemo, createSignal, Match, Switch, type ParentProps } from "solid-js";
import { classifyAuthFailure } from "../connect.ts";
import { bootstrapDashboardAccess } from "../store/dashboard-selection.ts";
import { hasConfirmedDashboardAccess, rootStore } from "../store/root.ts";
import { ROUTES } from "../routes.ts";
import {
  deploymentMode,
  isManagedPublicRoute,
  managedRouteDecision,
  type ManagedAccessState,
} from "../auth/managed-routes.ts";
import { ManagedAuthLayout } from "./ManagedAuthLayout.tsx";
import { Button } from "./Settings/md/Button.tsx";

const DASHBOARD_ACCESS_PATH = "/roost.v1.CoordinatorService/AuthDashboardAccess";

export function ManagedRouteGate(props: ParentProps) {
  const location = useLocation();
  const [accessState, setAccessState] = createSignal<ManagedAccessState>("idle");
  let accessAttempt = 0;
  const hasDashboardAccess = createMemo(hasConfirmedDashboardAccess);

  createEffect(() => {
    const managed = rootStore.coord_identity?.saas_mode === true;
    const protectedRoute = !isManagedPublicRoute(location.pathname);
    if (!managed || !protectedRoute) {
      accessAttempt++;
      setAccessState("idle");
      return;
    }
    if (rootStore.browser_unauthorized) {
      accessAttempt++;
      setAccessState("unauthorized");
      return;
    }
    if (hasDashboardAccess()) {
      accessAttempt++;
      setAccessState("idle");
      return;
    }
    if (accessState() !== "idle") return;

    const attempt = ++accessAttempt;
    setAccessState("checking");
    void bootstrapDashboardAccess().then(
      (confirmed) => {
        if (attempt !== accessAttempt) return;
        setAccessState(confirmed && hasDashboardAccess() ? "idle" : "error");
      },
      (error: unknown) => {
        if (attempt !== accessAttempt) return;
        setAccessState(
          classifyAuthFailure(error, DASHBOARD_ACCESS_PATH) === "device"
            ? "unauthorized"
            : "error",
        );
      },
    );
  });

  const decision = createMemo(() => managedRouteDecision({
    mode: deploymentMode(rootStore.coord_identity?.saas_mode),
    pathname: location.pathname,
    hasDashboardAccess: hasDashboardAccess(),
    accessState: rootStore.browser_unauthorized ? "unauthorized" : accessState(),
  }));

  return (
    <Switch>
      <Match when={decision() === "render"}>{props.children}</Match>
      <Match when={decision() === "login"}>
        <Navigate href={ROUTES.LOGIN} />
      </Match>
      <Match when={decision() === "app"}>
        <Navigate href={ROUTES.APP} />
      </Match>
      <Match when={decision() === "self-hosted-home"}>
        <Navigate href="/" />
      </Match>
      <Match when={decision() === "error"}>
        <ManagedAuthLayout
          testId="managed-access-error"
          title="Roost couldn’t confirm your dashboard"
          description="Check your connection and try again."
        >
          <div class="managed-auth-status">
            <Button
              class="managed-auth-submit"
              type="button"
              variant="filled"
              onClick={() => setAccessState("idle")}
            >
              Try again
            </Button>
          </div>
        </ManagedAuthLayout>
      </Match>
      <Match when={true}>
        <ManagedAuthLayout
          testId="managed-access-loading"
          title="Checking your Roost access"
          description="Roost is confirming your dashboard access."
        >
          <div class="managed-auth-status" aria-busy="true">
            <p role="status" aria-live="polite">Checking your Roost access…</p>
          </div>
        </ManagedAuthLayout>
      </Match>
    </Switch>
  );
}
