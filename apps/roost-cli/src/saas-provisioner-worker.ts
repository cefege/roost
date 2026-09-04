/**
 * Preserves the top-level worker boundary used by the root-side SaaS provisioner runtime.
 * The implementation remains under the existing SaaS module and is re-exported unchanged.
 * Keeping this facade thin avoids duplicating privileged provisioning or assertion validation.
 */

export * from "./saas/provisioner-worker.ts";
