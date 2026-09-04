// Validates opaque tenant routing keys at shared trust boundaries.
// Coordinators and edge-facing callers use the same lowercase hexadecimal contract.
// A narrow type guard keeps malformed route identifiers out of downstream routing.

export function isTenantRouteKey(value: unknown): value is string {
  return typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
}
