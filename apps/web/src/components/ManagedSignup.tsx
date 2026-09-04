// The public signup route resolves coordinator policy before mounting enrollment controls.
// Loading and gateway failures stay distinct from a settled, explicit signup denial.
// The enabled component exclusively owns Turnstile, email, and Google side effects.

import { createMemo, createResource, Match, Switch } from "solid-js";
import { getManagedAuthConfig } from "../auth/managed-auth-gateway.ts";
import { managedSignupRouteEnabled } from "../auth/managed-signup-policy.ts";
import { ROUTES } from "../routes.ts";
import { ManagedSignupEnabled } from "./ManagedSignupEnabled.tsx";
import { ManagedSignupPolicyState } from "./ManagedSignupPolicyState.tsx";

export function ManagedSignup() {
  const [config, { refetch }] = createResource(() => getManagedAuthConfig());
  const enabledConfig = createMemo(() => {
    if (config.loading || config.error) return undefined;
    const resolved = config();
    return managedSignupRouteEnabled(ROUTES.SIGNUP, resolved) ? resolved : undefined;
  });
  const invalidSettledConfig = createMemo(() => {
    if (config.loading || config.error) return false;
    const resolved = config();
    return resolved === undefined
      || (resolved.signupEnabled && !managedSignupRouteEnabled(ROUTES.SIGNUP, resolved));
  });

  return (
    <Switch fallback={
      <ManagedSignupPolicyState
        testId="managed-signup"
        state="unavailable"
      />
    }>
      <Match when={config.loading}>
        <ManagedSignupPolicyState testId="managed-signup" state="loading" />
      </Match>
      <Match when={config.error !== undefined || invalidSettledConfig()}>
        <ManagedSignupPolicyState
          testId="managed-signup"
          state="error"
          onRetry={() => { void refetch(); }}
        />
      </Match>
      <Match when={enabledConfig()} keyed>
        {(resolved) => <ManagedSignupEnabled config={resolved} />}
      </Match>
    </Switch>
  );
}
