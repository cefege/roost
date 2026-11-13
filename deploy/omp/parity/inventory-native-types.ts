import type { Origin } from "./inventory-source";

/** Shapes of a terminal-part proof, shared by the graph that derives it and the classifier that records it. */
export interface PartUse extends Origin {
	owner: string;
	member: string;
	memberKind: "terminal" | "native" | "input" | "other";
	nativeReachable: boolean;
	terminalReachable: boolean;
	/** Why a use in a member a native hook runs still executes no hook of the part. */
	hookInert?: string;
}
export interface OwnerHook extends Origin { name: string; endLine: number }
export interface PartOwner {
	id: string;
	/** Components between the part and this owner, nearest first; each holds the part only through terminal hooks. */
	chain: string[];
	construction: Origin & { expression: string };
	hops: Array<Origin & { expression: string }>;
	uses: PartUse[];
	nativeHooks: OwnerHook[];
}
export interface TerminalPartProof { class: "terminal-part" | "native-held-part"; owners: PartOwner[] }
export interface AbsenceRejection { reason: string; origin?: Origin }
export type TerminalPartResult = PartOwner[] | AbsenceRejection;
