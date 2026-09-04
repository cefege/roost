// Defines the ports and value shapes that bound SaaS lifecycle orchestration.
// Lifecycle stages share these contracts without depending on concrete Docker code.
// The bootstrap distinctions keep credential setup explicit through every retry.
import type { RegistryAccount, RegistryCoordinator } from "./registry.ts";
import type {
  ActivationStatus,
  ManagedEmailRuntimeConfig,
  ManagedInstanceSpec,
} from "./docker.ts";
import type { SaasRegistry } from "./registry.ts";

export const ROUTED_STATES = ["routed", "invited", "active"] as const;
export const OPERATION_LEASE_MS = 5 * 60_000;
export const ROUTE_LEASE_MS = 5 * 60_000;

export interface TenantRouteManager {
  reconcile(coordinators: readonly RegistryCoordinator[]): Promise<void>;
  verify(coordinator: RegistryCoordinator): Promise<void>;
  verifyResolver(account: RegistryAccount): Promise<void>;
}

export interface ProvisioningAdmission {
  assertBeforeReservation<T>(reservation: () => T): Promise<T>;
  assertPendingWorkAllowed(): Promise<void>;
}

export interface ManagedRuntimePort {
  seedOwnerActivation(spec: ManagedInstanceSpec): Promise<void>;
  seedSignupGatewayOwnerActivation(
    spec: ManagedInstanceSpec,
    activationTokenHash: string,
  ): Promise<void>;
  seedGoogleOwner(spec: ManagedInstanceSpec, googleSubject: string): Promise<void>;
  ensureContainer(spec: ManagedInstanceSpec): Promise<void>;
  startAndVerify(spec: ManagedInstanceSpec, timeoutMs?: number): Promise<void>;
  stop(coordinator: RegistryCoordinator): Promise<void>;
  releaseOwnerActivationEmail(coordinator: RegistryCoordinator): Promise<void>;
  recoverInterruptedReplacement?(spec: ManagedInstanceSpec): Promise<void>;
  activationStatus(coordinator: RegistryCoordinator): Promise<ActivationStatus>;
}

export interface SaasLifecycleOptions {
  registry: SaasRegistry;
  runtime: ManagedRuntimePort;
  routes: TenantRouteManager;
  email: ManagedEmailRuntimeConfig;
  authVerifyKeyFile: string;
  admission: ProvisioningAdmission;
  now?: () => number;
  leaseOwner?: () => string;
}
export type ProvisioningBootstrap =
  | { kind: "verified-email"; activationTokenHash: string }
  | { kind: "google"; subject: string };
export type LifecycleBootstrap =
  | ProvisioningBootstrap
  | { kind: "operator-email" };


export interface ProvisioningResult {
  account: RegistryAccount;
  coordinator: RegistryCoordinator;
  resumed: boolean;
}

export interface ReconcileResult {
  coordinatorId: string;
  state: string;
  repaired: boolean;
  error?: string;
}

export function redactLifecycleError(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error);
  return raw
    .replace(/(?:Bearer\s+|token=)[^\s&#]+/gi, "[credential]")
    .replace(/[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}/gi, "[email]")
    .replace(/[A-Za-z0-9_-]{40,}/g, "[secret]")
    .replace(/[\u0000-\u001f\u007f-\u009f]/gu, " ")
    .slice(0, 1_024) || "managed lifecycle failed";
}

export function specFor(
  account: RegistryAccount,
  coordinator: RegistryCoordinator,
  email: ManagedEmailRuntimeConfig,
  authVerifyKeyFile: string,
): ManagedInstanceSpec {
  if (coordinator.accountId !== account.id) throw new Error("registry account/coordinator mismatch");
  if (coordinator.routeKey !== account.routeKey) throw new Error("registry account/coordinator route key mismatch");
  return { account, coordinator, authVerifyKeyFile, email };
}
