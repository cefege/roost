// Fail-closed feature policy for public managed account-creation surfaces.
// Login, signup, verification, and Google completion share these exact decisions.
// Only validated gateway configuration can expose a provider or start enrollment.

import type { ManagedAuthConfig } from "./managed-auth-gateway.ts";
import { ROUTES } from "../routes.ts";

export const MANAGED_SIGNUP_UNAVAILABLE_MESSAGE =
  "Account creation is unavailable. Contact your Roost operator.";

export function managedSignupRouteEnabled(
  pathname: string,
  config: ManagedAuthConfig | null | undefined,
): boolean {
  if (config?.signupEnabled !== true) return false;
  if (pathname === ROUTES.SIGNUP_VERIFY) return true;
  return pathname === ROUTES.SIGNUP && config.turnstileSiteKey.trim().length > 0;
}

export function managedGoogleEnabled(
  config: ManagedAuthConfig | null | undefined,
): boolean {
  return config?.googleEnabled === true;
}
