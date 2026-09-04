// Same-tab cleanup progress for managed Google and email-signup completion.
// Auth flows use it to avoid repeating destructive tenant cleanup after retries.
// Logout clears both memory and session storage so another account inherits nothing.

import { isTenantRouteKey } from "@roost/shared/tenant-route";

const GOOGLE_PROGRESS_KEY = "roost.googleCompletion.v1";
const EMAIL_SIGNUP_PROGRESS_KEY = "roost.emailSignupActivation.v1";

type ProgressKind = "google" | "email-signup";

const progressKeys: Readonly<Record<ProgressKind, string>> = {
  google: GOOGLE_PROGRESS_KEY,
  "email-signup": EMAIL_SIGNUP_PROGRESS_KEY,
};
const progressRoutes: Record<ProgressKind, string | null> = {
  google: null,
  "email-signup": null,
};

function storedProgressRoute(kind: ProgressKind): string | null {
  if (progressRoutes[kind]) return progressRoutes[kind];
  const key = progressKeys[kind];
  try {
    const raw = sessionStorage.getItem(key);
    if (raw === null) return null;
    const value: unknown = JSON.parse(raw);
    if (
      value
      && typeof value === "object"
      && "routeKey" in value
      && "cleanupCompleted" in value
      && isTenantRouteKey(value.routeKey)
      && value.cleanupCompleted === true
    ) {
      progressRoutes[kind] = value.routeKey;
      return value.routeKey;
    }
    sessionStorage.removeItem(key);
  } catch {
    // Module memory remains the private-mode fallback for this document.
  }
  return null;
}

function recordProgressRoute(kind: ProgressKind, routeKey: string): void {
  progressRoutes[kind] = routeKey;
  try {
    sessionStorage.setItem(progressKeys[kind], JSON.stringify({
      routeKey,
      cleanupCompleted: true,
    }));
  } catch {
    // Module memory carries progress until this document closes.
  }
}

function clearProgress(kind: ProgressKind): void {
  progressRoutes[kind] = null;
  try { sessionStorage.removeItem(progressKeys[kind]); } catch { /* unavailable */ }
}

export function managedGoogleCleanupCompleted(routeKey: string): boolean {
  return storedProgressRoute("google") === routeKey;
}

export function recordManagedGoogleCleanup(routeKey: string): void {
  recordProgressRoute("google", routeKey);
}

export function clearManagedGoogleCompletionProgress(): void {
  clearProgress("google");
}

export function managedEmailSignupCleanupCompleted(routeKey: string): boolean {
  return storedProgressRoute("email-signup") === routeKey;
}

export function recordManagedEmailSignupCleanup(routeKey: string): void {
  recordProgressRoute("email-signup", routeKey);
}

export function clearManagedEmailSignupActivationProgress(): void {
  clearProgress("email-signup");
}

export function clearManagedAuthProgressForLogout(): void {
  clearProgress("google");
  clearProgress("email-signup");
}
