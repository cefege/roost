// This module owns the route policy separating managed and self-hosted entry flows.
// The application shell calls it while deployment mode and dashboard access are resolving.
// It depends only on canonical route constants so the decision table stays side-effect free.

import { ROUTES } from "../routes.ts";
export type DeploymentMode = "unknown" | "self-hosted" | "managed";
export type ManagedAccessState = "idle" | "checking" | "unauthorized" | "error";
export type ManagedRouteDecision =
  | "render"
  | "loading"
  | "login"
  | "app"
  | "self-hosted-home"
  | "error";

export function isManagedPublicRoute(pathname: string): boolean {
  return pathname === ROUTES.LOGIN
    || pathname === ROUTES.SIGNUP
    || pathname === ROUTES.SIGNUP_VERIFY
    || pathname === ROUTES.GOOGLE_COMPLETE
    || pathname === ROUTES.ACTIVATE
    || pathname === ROUTES.FORGOT_PASSWORD
    || pathname === ROUTES.RESET_PASSWORD;
}


export function deploymentMode(saasMode: boolean | undefined): DeploymentMode {
  if (saasMode === undefined) return "unknown";
  return saasMode ? "managed" : "self-hosted";
}

export function managedRouteDecision(input: {
  mode: DeploymentMode;
  pathname: string;
  hasDashboardAccess: boolean;
  accessState: ManagedAccessState;
}): ManagedRouteDecision {
  const publicManagedRoute = isManagedPublicRoute(input.pathname);
  const explicitManagedRoute = publicManagedRoute || input.pathname === ROUTES.APP;

  if (input.mode === "unknown") return "loading";

  if (input.mode === "self-hosted") {
    return explicitManagedRoute ? "self-hosted-home" : "render";
  }

  if (publicManagedRoute) {
    if (input.pathname === ROUTES.LOGIN) {
      return input.hasDashboardAccess && input.accessState !== "unauthorized" ? "app" : "render";
    }
    return "render";
  }
  if (input.accessState === "unauthorized") return "login";
  if (input.hasDashboardAccess) {
    return input.pathname === ROUTES.ROOT || input.pathname === ROUTES.PAIR
      ? "app"
      : "render";
  }
  if (input.accessState === "error") return "error";
  return "loading";
}
