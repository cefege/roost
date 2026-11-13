import type { InventoryEntry } from "./inventory-types";

const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const located = (value: unknown, source: string, pattern: RegExp): boolean => record(value) && value.source === source && Number.isInteger(value.line) && Number(value.line) > 0
  && typeof value.expression === "string" && pattern.test(value.expression);

/**
 * A declared `null` may leave an element prop's demanded domain only through
 * the located source proof that the document never stores a top-level null:
 * the node builder omits it, `add` skips it and `set` deletes the key. A
 * domain that declares null and lacks both the value and that proof still owes
 * observation of null.
 */
export function nullDomainReasons(entry: InventoryEntry): string[] {
  const domain = entry.details.enumDomain;
  if (!record(domain) || !Array.isArray(domain.types) || !domain.types.includes("null")) return [];
  const values = Array.isArray(entry.details.enumValues) ? entry.details.enumValues : [];
  if (values.includes(null)) return [];
  const excluded = Array.isArray(domain.excludedValues) ? domain.excludedValues.filter(item => record(item) && item.value === null) : [];
  const proof = excluded.length === 1 && record(excluded[0]) && excluded[0].reason === "top-level-null-is-absent-prop" ? excluded[0].proof : undefined;
  const sound = entry.details.exposure === "ui" && record(proof)
    && located(proof.producerWire, "packages/tui/src/native/reconcile.ts", /!==\s*undefined[\s\S]*!==\s*null|!==\s*null[\s\S]*!==\s*undefined/)
    && located(proof.addBuild, "packages/tui/src/native/apply.ts", /!==\s*undefined[\s\S]*!==\s*null|!==\s*null[\s\S]*!==\s*undefined/)
    && located(proof.setClear, "packages/tui/src/native/apply.ts", /===\s*null[\s\S]*delete\b/);
  return sound ? [] : ["Source null value excluded without its located top-level absence proof"];
}
