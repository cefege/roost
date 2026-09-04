// Exposes the complete SaaS registry while domain stores own its implementation.
// CLI lifecycle, rollout, resolver, and provisioning code import this stable facade.
// The facade preserves the existing API and durable SQLite format across splits.
import { RegistryLeaseStore } from "./registry-lease-store.ts";

export * from "./registry-model.ts";
export {
  SaasRegistryError,
  assertCanonicalGoogleIssuer,
  assertCanonicalUuid,
  assertGoogleIdentitySubject,
  assertImmutableImageDigest,
  assertSha256Hex,
  assertTenantRouteKey,
  coordinatorContainerName,
  coordinatorDataDir,
  coordinatorHostname,
  createTenantRouteKey,
} from "./registry-validation.ts";

export class SaasRegistry extends RegistryLeaseStore {}
