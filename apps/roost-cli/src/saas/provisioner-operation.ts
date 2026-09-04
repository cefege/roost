// Builds the privileged provisioning operation served over private IPC.
// The SaaS provisioner server calls this factory once after host checks pass.
// Requests are translated into canonical worker submissions and responses.
import type { CanonicalJsonValue } from "../saas-auth/canonical-json.ts";
import type { PrivateIpcRequest } from "../saas-auth/private-ipc.ts";
import type { ProvisionerOperation } from "../saas-provisioner/server.ts";
import { CaddyTenantRouter } from "./caddy.ts";
import { ManagedInstanceRuntime } from "./docker.ts";
import {
  HostAdmission,
  assertSaasProvisionerStartupPrerequisites,
  loadSaasHostConfig,
} from "./host.ts";
import type { TenantRouteManager } from "./lifecycle.ts";
import { SaasLifecycle } from "./lifecycle.ts";
import { ManagedRouteProbe } from "./probe.ts";
import { SaasRegistry } from "./registry.ts";
import { MAX_GOOGLE_PROOF_LIFETIME_MS, type ProvisioningStatus, type ProvisioningSubmitResult } from "./provisioning-contract.ts";
import { verifyLinkTicket } from "./provisioning-link-ticket.ts";
import { ProvisioningWorker } from "./provisioning-submission-worker.ts";

function canonicalOperationResponse(
  value: ProvisioningSubmitResult | ProvisioningStatus,
): CanonicalJsonValue {
  return { ok: true, ...value } as CanonicalJsonValue;
}

export interface ProvisionerOperationRuntime {
  operation: ProvisionerOperation;
  close(): Promise<void>;
}

export async function createProvisionerOperation(): Promise<ProvisionerOperationRuntime> {
  const config = loadSaasHostConfig(process.env as Record<string, string | undefined>);
  await assertSaasProvisionerStartupPrerequisites(config);
  const registry = new SaasRegistry({ rootDir: config.rootDir, path: config.registryPath });
  const runtime = new ManagedInstanceRuntime({ network: config.network });
  const caddy = new CaddyTenantRouter({ confDir: config.caddyConfDir });
  const probe = new ManagedRouteProbe();
  const routes: TenantRouteManager = {
    reconcile: (coordinators) => caddy.reconcile(coordinators),
    verify: (coordinator) => probe.verify(coordinator),
    verifyResolver: (account) => probe.verifyResolver(account),
  };
  const admission = new HostAdmission({
    registry,
    config,
    onAlert: (message) => console.error(JSON.stringify({
      event: "saas.admission_warning",
      message,
    })),
  });
  const lifecycle = new SaasLifecycle({
    registry,
    runtime,
    routes,
    admission,
    email: {
      resendEndpoint: config.resendEndpoint,
      emailFrom: config.emailFrom,
      sharedResendApiKeyPath: config.sharedResendApiKeyPath,
    },
    authVerifyKeyFile: config.authVerifyKeyFile,
  });
  const worker = new ProvisioningWorker({
    registry,
    lifecycle,
    admission,
    imageDigest: config.imageDigest,
  });
  const operation: ProvisionerOperation = async (request: PrivateIpcRequest) => {
    if (request.purpose === "status") {
      return canonicalOperationResponse(worker.status(request.body.jobId));
    }
    if (request.purpose === "finalize-link") {
      return canonicalOperationResponse(await worker.finalizeLink(request.body.jobId));
    }
    const { kind, submission } = request.body;
    if (kind === "verified-email") {
      return canonicalOperationResponse(await worker.submit({ kind, ...submission }));
    }
    const expiresAtMs = submission.verifiedAtMs + MAX_GOOGLE_PROOF_LIFETIME_MS;
    if (kind === "google-link") {
      const ticket = await verifyLinkTicket(registry, submission, Date.now());
      return canonicalOperationResponse(await worker.submit({
        kind,
        issuer: submission.identityIssuer,
        subject: submission.identitySubject,
        emailNormalized: submission.emailNormalized,
        verifiedAtMs: submission.verifiedAtMs,
        expiresAtMs,
        idempotencyKey: submission.idempotencyKey,
        ticket,
      }));
    }
    return canonicalOperationResponse(await worker.submit({
      kind,
      issuer: submission.identityIssuer,
      subject: submission.identitySubject,
      emailNormalized: submission.emailNormalized,
      verifiedAtMs: submission.verifiedAtMs,
      expiresAtMs,
      idempotencyKey: submission.idempotencyKey,
    }));
  };
  try {
    await lifecycle.reconcile();
    worker.start();
  } catch (error) {
    registry.close();
    throw error;
  }
  return {
    operation,
    async close(): Promise<void> {
      try {
        await worker.stop();
      } finally {
        registry.close();
      }
    },
  };
}
