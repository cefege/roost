// Exposes the complete SaaS lifecycle while stage modules own its implementation.
// CLI commands, rollout, and provisioning import this stable orchestration facade.
// Inherited operations preserve the established API and side-effect ordering.
import { LifecycleReconciliation } from "./lifecycle-reconciliation.ts";

export type {
  ManagedRuntimePort,
  ProvisioningAdmission,
  ProvisioningBootstrap,
  ProvisioningResult,
  ReconcileResult,
  SaasLifecycleOptions,
  TenantRouteManager,
} from "./lifecycle-contract.ts";

export class SaasLifecycle extends LifecycleReconciliation {}
