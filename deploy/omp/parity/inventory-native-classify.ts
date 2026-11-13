import type { InventoryEntry } from "./inventory-types";
import type { SourceIndex } from "./inventory-source";
import { ComponentGraph } from "./inventory-native-absence";
import type { AbsenceRejection, TerminalPartProof } from "./inventory-native-types";
import { nativeTransportProof } from "./inventory-native-transport";
import { cliUnreachableProof, composerStyleProof, importGraph, inputListenerProof } from "./inventory-native-classes";

/**
 * Components whose native description source never requests. Each is rewritten
 * to its class with a located proof the coverage gate re-validates; anything
 * without a sound proof keeps its source status and records why it was refused.
 */
export const NATIVE_ABSENCE_STATUSES = ["native-hook-absent", "rows-fallback", "null-description"];

export function classifyNativeAbsence(source: SourceIndex, entries: InventoryEntry[]): void {
	const absent = entries.filter(entry => entry.category === "component" && NATIVE_ABSENCE_STATUSES.includes(String(entry.details.nativeStatus)));
	if (!absent.length) return;
	const graph = new ComponentGraph(source, entries);
	const imports = importGraph(source);
	for (const entry of absent) {
		const node = graph.nodeOf(entry);
		const refusals: string[] = [];
		const attempts = [
			() => node ? composerStyleProof(source, entry, node) : { reason: "component node is not in the source index" },
			() => entry.source.startsWith("packages/coding-agent/src/cli/gallery-") ? cliUnreachableProof(imports, entry) : { reason: "not a CLI fixture module" },
			() => node ? nativeTransportProof(source, entry, node) : { reason: "component node is not in the source index" },
			() => {
				const owners = graph.resolve(entry);
				return Array.isArray(owners) ? { class: "terminal-part", owners } satisfies TerminalPartProof : owners;
			},
			() => inputListenerProof(entry),
			() => {
				const owners = graph.resolve(entry, true);
				return Array.isArray(owners) ? { class: "native-held-part", owners } satisfies TerminalPartProof : owners;
			},
		];
		let origin: AbsenceRejection["origin"];
		const classified = attempts.some(attempt => {
			const outcome = attempt();
			if (!("class" in outcome)) { refusals.push(outcome.reason); origin = outcome.origin ?? origin; return false; }
			entry.details.nativeStatusInSource = entry.details.nativeStatus;
			entry.details.nativeStatus = outcome.class;
			entry.details.nativeAbsence = outcome;
			return true;
		});
		if (!classified) entry.details.nativeAbsenceRefusal = { reasons: refusals, origin };
	}
}
