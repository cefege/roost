//! Marks every feature check the Roost browser host never negotiates: the host's
//! single `open` action lists the features it advertises, so a check for any other
//! name always answers false there. Read by `coverage-provenance.ts`.
import { readFileSync } from "node:fs";
import path from "node:path";
import { ROOST_ROOT, RUST_HOST, rustFiles } from "./inventory-host-audit";
import type { InventoryEntry } from "./inventory-types";

export interface HostFeatures { features: Set<string>; source: string; line: number }

/** The features the browser's single `open` action advertises, with that action's source line. */
export function hostAdvertisedFeatures(root = ROOST_ROOT): HostFeatures {
	const opens = RUST_HOST.flatMap(directory => rustFiles(path.join(root, directory))).flatMap(file => {
		const text = readFileSync(file, "utf8");
		return [...text.matchAll(/"method":"open","params":\{[^}]*"features":\[([^\]]*)\]/g)].map(match => ({
			features: new Set([...match[1]!.matchAll(/"([^"]+)"/g)].map(feature => feature[1]!)),
			source: path.relative(root, file).split(path.sep).join("/"),
			line: text.slice(0, match.index).split("\n").length,
		}));
	});
	if (opens.length !== 1) throw new Error(`Expected one browser open action advertising features; found ${opens.length}`);
	return opens[0]!;
}

/** Attach the host's advertisement to each `feature:<name>` the browser never negotiates. */
export function classifyUnadvertisedFeatures(entries: InventoryEntry[], host: HostFeatures = hostAdvertisedFeatures()): void {
	for (const entry of entries) {
		if (entry.category !== "feature" || typeof entry.details.name !== "string" || host.features.has(entry.details.name)) continue;
		entry.details.hostFeatures = { advertised: [...host.features].sort(), source: host.source, line: host.line };
	}
}
