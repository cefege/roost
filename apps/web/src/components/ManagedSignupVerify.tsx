// The email-verification route resolves coordinator policy before reading signup credentials.
// Loading and gateway failures stay distinct from a settled, explicit signup denial.
// The enabled component exclusively owns verification, polling, and provisioning effects.

import { createMemo, createResource, Match, Switch } from "solid-js";
import { getManagedAuthConfig } from "../auth/managed-auth-gateway.ts";
import { managedSignupRouteEnabled } from "../auth/managed-signup-policy.ts";
import { ROUTES } from "../routes.ts";
import { ManagedSignupPolicyState } from "./ManagedSignupPolicyState.tsx";
import { ManagedSignupVerifyEnabled } from "./ManagedSignupVerifyEnabled.tsx";

export function ManagedSignupVerify() {
  const [config, { refetch }] = createResource(() => getManagedAuthConfig());
  const enabledConfig = createMemo(() => {
    if (config.loading || config.error) return undefined;
    const resolved = config();
    return managedSignupRouteEnabled(ROUTES.SIGNUP_VERIFY, resolved) ? resolved : undefined;
  });

  return (
    <Switch fallback={
      <ManagedSignupPolicyState
        testId="managed-signup-verify"
        state="unavailable"
      />
    }>
      <Match when={config.loading}>
        <ManagedSignupPolicyState testId="managed-signup-verify" state="loading" />
      </Match>
      <Match when={config.error !== undefined || (!config.loading && config() === undefined)}>
        <ManagedSignupPolicyState
          testId="managed-signup-verify"
          state="error"
          onRetry={() => { void refetch(); }}
        />
      </Match>
      <Match when={enabledConfig()} keyed>
        {(_resolved) => <ManagedSignupVerifyEnabled />}
      </Match>
    </Switch>
  );
}
