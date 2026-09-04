/**
 * Assembles and owns the signup gateway's production authentication dependencies.
 * The hidden CLI entry point calls this module to start HTTP, email, state, and IPC services.
 * Centralized startup and teardown ensure secrets and durable stores share one lifecycle.
 */

import { createPrivateKey } from "node:crypto";
import { createResendEmailClient } from "@roost/shared/email-client";
import { EmailSignupProtocol } from "./email-signup.ts";
import { FederatedAssertionSigner } from "./federated-assertion.ts";
import { loadGatewayConfig } from "./gateway-config.ts";
import { GoogleOAuthProtocol } from "./google-oauth.ts";
import { startSaasAuthServer } from "./http-server.ts";
import { createPrivateProvisionerClient } from "./provisioner-client.ts";
import { ResultProtocol } from "./result-protocol.ts";
import { SignupEmailOutbox } from "./signup-email-outbox.ts";
import { GatewayStateStore } from "./state-store.ts";
import { TurnstileVerifier } from "./turnstile.ts";

export async function serveSaasAuth(): Promise<void> {
  const config = loadGatewayConfig();
  const signingKey = createPrivateKey(config.credentials["assertion-signing-key"]);
  const store = new GatewayStateStore({
    path: config.stateDatabasePath,
    oauthStateKey: config.credentials["oauth-state-key"],
  });
  const provisioner = createPrivateProvisionerClient(signingKey);
  const outbox = new SignupEmailOutbox({
    store,
    emailOutboxKey: config.credentials["email-outbox-key"],
    client: createResendEmailClient({
      endpoint: config.outbound.resendEmails,
      apiKey: config.credentials["resend-api-key"],
      from: config.emailFrom,
    }),
  });
  const turnstile = new TurnstileVerifier({
    store,
    secret: config.credentials["turnstile-secret"],
  });
  const email = new EmailSignupProtocol({
    store,
    outbox,
    turnstile,
    provisioner,
    signupEnabled: config.signupEnabled,
  });
  const google = new GoogleOAuthProtocol({
    store,
    turnstile,
    provisioner,
    googleEnabled: config.googleEnabled,
    signupEnabled: config.signupEnabled,
    clientId: config.googleClientId,
    clientSecret: config.credentials["google-client-secret"],
  });
  const result = new ResultProtocol({
    store,
    provisioner,
    signer: new FederatedAssertionSigner({ signingKey }),
  });
  const emailTimer = config.signupEnabled
    ? setInterval(() => {
      void outbox.runOnce().catch(() => undefined);
    }, 1_000)
    : undefined;
  emailTimer?.unref?.();
  const service = startSaasAuthServer({
    signupEnabled: config.signupEnabled,
    googleEnabled: config.googleEnabled,
    turnstileSiteKey: config.turnstileSiteKey,
    email,
    google,
    result,
  });
  console.log(JSON.stringify({
    event: "saas.auth_gateway_listening",
    host: config.listenHost,
    port: config.listenPort,
  }));
  try {
    await new Promise<void>((resolve) => {
      process.once("SIGINT", resolve);
      process.once("SIGTERM", resolve);
    });
  } finally {
    clearInterval(emailTimer);
    service.stop();
    store.close();
  }
}

export async function saasAuth(args: string[]): Promise<void> {
  if (args.length !== 1 || args[0] !== "serve") throw new Error("internal SaaS auth dispatch refused");
  await serveSaasAuth();
}
