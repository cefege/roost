import type { InventoryEntry } from "./inventory-types";

/**
 * An abstract class is never a receiver, so its hooks run only as its concrete
 * subclasses; recording those subclasses lets coverage demand the hooks on them.
 */
export function recordConcreteSubclasses(entries: InventoryEntry[]): void {
	const components = entries.filter(entry => entry.category === "component");
	const derivedFrom = (entry: InventoryEntry, ancestor: string, seen: Record<string, true>): boolean => {
		if (seen[entry.id]) return false;
		seen[entry.id] = true;
		return (Array.isArray(entry.details.bases) ? entry.details.bases : []).some(base => base.declaration === ancestor
			|| components.some(parent => parent.id === base.declaration && derivedFrom(parent, ancestor, seen)));
	};
	for (const entry of components) {
		if (entry.details.abstract !== true) continue;
		entry.details.concreteSubclasses = components.filter(candidate => candidate.details.abstract !== true && derivedFrom(candidate, entry.id, {})).map(candidate => candidate.id);
	}
}
