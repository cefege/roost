import type { InventoryEntry } from "./inventory-types";
import type { ProbeVisit } from "./probe";

/**
 * Evidence for components whose native description source never requests
 * (`nativeStatus` is a class from `inventory-native-classify`). The class and
 * its structured proof are validated here, never trusted from the status
 * alone; each class then demands the execution that makes its proof bind.
 */
const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
interface Located { source: string; line: number }
const located = (value: unknown): value is Located & Record<string, unknown> => record(value) && typeof value.source === "string" && value.source.length > 0
	&& Number.isInteger(value.line) && Number(value.line) > 0;
const withText = (value: unknown, key: string): value is Located & Record<string, unknown> => located(value) && typeof value[key] === "string";
const NATIVE_HOOK = /^(?:describe|nativeSheet$)/;
const PLAIN_VISIT = (visit: ProbeVisit) => !["access", "feature"].includes(visit.method) && !visit.method.startsWith("return:");
const COMPOSER_CHROME = "packages/tui/src/components/composer/native-chrome.ts";
const COMPOSER_HOOKS = ["renderTop", "renderRow", "renderBottom"];

export const NATIVE_ABSENCE_CLASSES = ["terminal-part", "native-held-part", "composer-style", "cli-unreachable", "native-transport", "input-listener"];
export const absenceClass = (details: Record<string, unknown>): string | undefined => NATIVE_ABSENCE_CLASSES.includes(String(details.nativeStatus)) ? String(details.nativeStatus) : undefined;

interface OwnerHook extends Located { name: string; endLine: number }
interface Owner { id: string; nativeHooks: OwnerHook[]; heldNatively: boolean }

function ownerReasons(entry: InventoryEntry, owners: unknown, held: boolean): { reasons: string[]; owners: Owner[] } {
	if (!Array.isArray(owners) || !owners.length) return { reasons: [`${held ? "native-held-part" : "terminal-part"} proof lists no owner`], owners: [] };
	const reasons: string[] = [];
	const valid: Owner[] = [];
	for (const owner of owners) {
		const problems: string[] = [];
		let heldNatively = false;
		if (!record(owner) || typeof owner.id !== "string" || !/^(?:component|render-helper):/.test(owner.id) || owner.id === entry.id) problems.push("owner identity");
		else {
			if (!withText(owner.construction, "expression")) problems.push("construction location");
			if (!Array.isArray(owner.hops) || !owner.hops.every(hop => withText(hop, "expression"))) problems.push("call-chain locations");
			if (!Array.isArray(owner.chain) || !owner.chain.every(link => typeof link === "string")) problems.push("component chain");
			const uses = Array.isArray(owner.uses) ? owner.uses : [];
			heldNatively = uses.some(use => record(use) && use.nativeReachable === true && use.hookInert === undefined);
			if (!uses.length || !uses.every(use => located(use) && typeof use.member === "string" && (use.nativeReachable === false || use.nativeReachable === true && (held || typeof use.hookInert === "string" && use.hookInert.length > 0)))) problems.push("part-use locations (a use a native hook reaches is a proof only when shown hook-inert)");
			else if (!uses.some(use => use.terminalReachable === true || held && use.nativeReachable === true && use.hookInert === undefined)) problems.push(held ? "native-hook hold or terminal-hook reach of the part" : "terminal-hook reach of the part");
			const hooks = Array.isArray(owner.nativeHooks) ? owner.nativeHooks : [];
			if (!hooks.length || !hooks.every(hook => located(hook) && typeof hook.name === "string" && NATIVE_HOOK.test(hook.name) && Number.isInteger(hook.endLine) && Number(hook.endLine) >= hook.line)) problems.push("owner native hook locations");
		}
		if (problems.length) reasons.push(`Malformed ${held ? "native-held-part" : "terminal-part"} owner proof: ${problems.join(", ")}`);
		else valid.push({ id: String((owner as Record<string, unknown>).id), nativeHooks: (owner as { nativeHooks: OwnerHook[] }).nativeHooks, heldNatively });
	}
	if (held && !reasons.length && !valid.some(owner => owner.heldNatively)) reasons.push("Malformed native-held-part proof: no owner's native hook holds the part");
	return { reasons, owners: valid };
}

const holds = (visit: ProbeVisit, id: string) => !id.startsWith("component:") || visit.receiverComponent === id.slice("component:".length) || visit.component === id.slice("component:".length);

/** The owner's own native hook ran on the browser leg and returned a descriptor from inside its body. */
function ownerDescribed(owner: Owner, visits: readonly ProbeVisit[]): boolean {
	return owner.nativeHooks.some(hook => visits.some(visit => visit.adapter === "browser" && visit.source === hook.source && visit.line === hook.line && PLAIN_VISIT(visit) && holds(visit, owner.id))
		&& visits.some(visit => visit.adapter === "browser" && visit.source === hook.source && visit.method === `return:${hook.name}` && visit.line >= hook.line && visit.line <= hook.endLine
			&& holds(visit, owner.id) && record(visit.arguments?.[0]) && visit.arguments[0].result === "value"));
}

function partRendered(entry: InventoryEntry, visits: readonly ProbeVisit[]): boolean {
	const identity = entry.id.slice("component:".length);
	const hooks = (Array.isArray(entry.details.hooks) ? entry.details.hooks : []).filter(record).filter(hook => typeof hook.name === "string" && /^render/.test(hook.name) && hook.referenced !== false);
	return hooks.length > 0 && hooks.every(hook => located(hook.implementation) && visits.some(visit => visit.adapter === "terminal" && visit.source === (hook.implementation as Located).source
		&& visit.line === (hook.implementation as Located).line && PLAIN_VISIT(visit) && (visit.receiverComponent === identity || !hook.inherited && visit.component === identity)));
}

/** A part its owner's native hook hands to the host runs there, so one of its own hooks must have executed on the browser leg. */
function partRanNatively(entry: InventoryEntry, visits: readonly ProbeVisit[]): boolean {
	const identity = entry.id.slice("component:".length);
	const hooks = (Array.isArray(entry.details.hooks) ? entry.details.hooks : []).filter(record).filter(hook => typeof hook.name === "string" && /^render/.test(hook.name) && hook.referenced !== false && located(hook.implementation));
	return hooks.some(hook => visits.some(visit => visit.adapter === "browser" && visit.source === (hook.implementation as Located).source && visit.line === (hook.implementation as Located).line
		&& PLAIN_VISIT(visit) && (visit.receiverComponent === identity || !hook.inherited && visit.component === identity)));
}

function terminalPartReasons(entry: InventoryEntry, proof: Record<string, unknown>, scenarios: readonly (readonly ProbeVisit[])[], held: boolean): string[] {
	const checked = ownerReasons(entry, proof.owners, held);
	if (checked.reasons.length) return checked.reasons;
	const together = scenarios.some(visits => partRendered(entry, visits) && (!held || partRanNatively(entry, visits)) && checked.owners.some(owner => (!held || owner.heldNatively) && ownerDescribed(owner, visits)));
	return together ? [] : [held ? "No scenario executes the part's render on both legs and its owner's native hook with a non-null descriptor" : "No scenario executes both the part's terminal render and its owner's native hook with a non-null descriptor"];
}

/** Native chrome calls the style's render hooks; only a call nested in one browser `describeComposerChrome` window is native execution. */
function insideChrome(visit: ProbeVisit, visits: readonly ProbeVisit[]): boolean {
	const same = (other: ProbeVisit) => other.adapter === "browser" && other.pid === visit.pid && other.generation === visit.generation && other.source === COMPOSER_CHROME;
	const opened = visits.filter(other => same(other) && other.method === "describeComposerChrome" && other.sequence < visit.sequence).reduce((latest, other) => Math.max(latest, other.sequence), -1);
	const closed = visits.filter(other => same(other) && other.method === "return:describeComposerChrome" && other.sequence > opened).reduce((first, other) => Math.min(first, other.sequence), Number.POSITIVE_INFINITY);
	return opened >= 0 && Number.isFinite(closed) && visit.sequence < closed;
}

function composerStyleReasons(entry: InventoryEntry, proof: Record<string, unknown>, scenarios: readonly (readonly ProbeVisit[])[]): string[] {
	const chrome = Array.isArray(proof.chrome) ? proof.chrome : [];
	const hooks = (Array.isArray(entry.details.hooks) ? entry.details.hooks : []).filter(record).filter(hook => typeof hook.name === "string" && COMPOSER_HOOKS.includes(hook.name) && hook.referenced !== false && located(hook.implementation));
	if (!hooks.length || !chrome.every(call => located(call) && call.source === COMPOSER_CHROME && typeof call.hook === "string")) return ["Malformed composer-style proof"];
	const missing = hooks.filter(hook => !chrome.some(call => record(call) && call.hook === hook.name));
	if (missing.length) return [`Composer-style proof has no native chrome call site for ${missing.map(hook => hook.name).join(", ")}`];
	const identity = entry.id.slice("component:".length);
	const ran = (visits: readonly ProbeVisit[], adapter: string, hook: Record<string, unknown>) => visits.some(visit => visit.adapter === adapter && visit.source === (hook.implementation as Located).source
		&& visit.line === (hook.implementation as Located).line && PLAIN_VISIT(visit) && (visit.receiverComponent === identity || visit.component === identity)
		&& (adapter === "terminal" || insideChrome(visit, visits)));
	return scenarios.some(visits => hooks.every(hook => ran(visits, "terminal", hook) && ran(visits, "browser", hook))) ? []
		: ["No scenario renders every style hook on the terminal and inside native chrome on the browser"];
}

function cliUnreachableReasons(entry: InventoryEntry, proof: Record<string, unknown>): string[] {
	const registration = proof.registration;
	const edges = Array.isArray(proof.edges) ? proof.edges : [];
	const launchers = Array.isArray(proof.launchers) ? proof.launchers : [];
	const problems: string[] = [];
	if (!withText(registration, "command") || typeof (registration as Record<string, unknown>).module !== "string") problems.push("command-table registration");
	if (!edges.length || !edges.every(edge => withText(edge, "importer") && typeof (edge as Record<string, unknown>).imported === "string")) problems.push("import chain");
	else if (!edges.some(edge => (edge as Record<string, unknown>).imported === entry.source)) problems.push("import chain from the fixture module");
	const roles = launchers.map(launcher => withText(launcher, "role") && withText(launcher, "text") && (launcher as Record<string, unknown>).repository === "roost" ? String((launcher as Record<string, unknown>).role) : "");
	if (!roles.includes("roost-worker") || !roles.includes("parity-terminal-leg")) problems.push("launcher argv locations for both Roost and the parity terminal leg");
	else if (withText(registration, "command") && launchers.some(launcher => (launcher as Record<string, string>).text.includes(`"${(registration as Record<string, string>).command}"`))) problems.push("launcher that passes the registered subcommand");
	return problems.length ? [`Malformed cli-unreachable proof: ${problems.join(", ")}`] : [];
}

/** The transport is built only behind a negotiated hello, so the browser leg is the only place its hooks run: each referenced hook must have executed there. */
function nativeTransportReasons(entry: InventoryEntry, proof: Record<string, unknown>, scenarios: readonly (readonly ProbeVisit[])[]): string[] {
	const hooks = (Array.isArray(entry.details.hooks) ? entry.details.hooks : []).filter(record).filter(hook => typeof hook.name === "string" && hook.referenced !== false);
	const calls = Array.isArray(proof.hookCalls) ? proof.hookCalls : [];
	const callers = Array.isArray(proof.callers) ? proof.callers : [];
	const component = proof.component;
	const construction = proof.construction;
	const problems: string[] = [];
	if (!withText(component, "name") || component.renderReturns !== "void" || !located(component.contract)) problems.push("class and Component-contract locations (render must return void)");
	if (!withText(construction, "expression") || typeof construction.holder !== "string" || typeof construction.method !== "string" || typeof construction.helloParameter !== "string") problems.push("construction location");
	else if (located(component) && construction.source === component.source) problems.push("construction outside the class's own module");
	if (!callers.length || !callers.every(caller => withText(caller, "expression"))) problems.push("construction-method caller locations");
	if (!calls.length || !calls.every(call => withText(call, "expression") && typeof call.hook === "string" && withText(call.guard, "expression"))) problems.push("guarded hook-call locations");
	else if (hooks.some(hook => !calls.some(call => record(call) && call.hook === hook.name))) problems.push("a host call site for every hook");
	if (problems.length) return [`Malformed native-transport proof: ${problems.join(", ")}`];
	const identity = entry.id.slice("component:".length);
	const ran = (visits: readonly ProbeVisit[], hook: Record<string, unknown>) => {
		const implementation = hook.implementation;
		return located(implementation) && visits.some(visit => visit.adapter === "browser" && visit.source === implementation.source
			&& visit.line === implementation.line && PLAIN_VISIT(visit) && (visit.receiverComponent === identity || visit.component === identity));
	};
	return hooks.length && scenarios.some(visits => hooks.every(hook => ran(visits, hook))) ? [] : ["No scenario executes every transport hook on the browser leg"];
}

/** A key listener has nothing to paint, so its witness is the input path: one scenario must deliver input to every hook on the terminal leg and through the ACP session on the browser leg. */
function inputListenerReasons(entry: InventoryEntry, proof: Record<string, unknown>, scenarios: readonly (readonly ProbeVisit[])[]): string[] {
	const identity = entry.id.slice("component:".length);
	const declared = (Array.isArray(entry.details.hooks) ? entry.details.hooks : []).filter(record).filter(hook => typeof hook.name === "string" && hook.referenced !== false && located(hook.implementation));
	const claimed = Array.isArray(proof.hooks) ? proof.hooks.filter(record) : [];
	if (!claimed.length || claimed.length !== declared.length || !claimed.every(claim => typeof claim.name === "string" && /^(?:handleInput|handleNativeEvent|handleMouse|handleKey|onKey|onMouse)/.test(claim.name)
		&& declared.some(hook => hook.name === claim.name && (hook.implementation as Located).source === claim.source && (hook.implementation as Located).line === claim.line))) return ["Malformed input-listener proof: located input hooks that are exactly the component's hooks"];
	const ran = (visits: readonly ProbeVisit[], claim: Record<string, unknown>, adapter: ProbeVisit["adapter"]) => visits.some(visit => visit.adapter === adapter && visit.method === claim.name
		&& visit.source === claim.source && visit.line === claim.line && PLAIN_VISIT(visit) && (visit.receiverComponent === identity || visit.component === identity));
	return scenarios.some(visits => claimed.every(claim => ran(visits, claim, "terminal") && ran(visits, claim, "browser"))) ? [] : ["No scenario delivers input to every hook on both the terminal leg and the browser leg"];
}

/** Reasons the entry's class proof or its required execution is missing; empty only when both hold. */
export function nativeAbsenceReasons(entry: InventoryEntry, scenarios: readonly (readonly ProbeVisit[])[]): string[] {
	const proof = entry.details.nativeAbsence;
	const kind = absenceClass(entry.details);
	if (!kind || !record(proof) || proof.class !== kind) return [`Unsupported native source state: ${entry.details.nativeStatus}`];
	if (kind === "terminal-part" || kind === "native-held-part") return terminalPartReasons(entry, proof, scenarios, kind === "native-held-part");
	if (kind === "composer-style") return composerStyleReasons(entry, proof, scenarios);
	if (kind === "native-transport") return nativeTransportReasons(entry, proof, scenarios);
	if (kind === "input-listener") return inputListenerReasons(entry, proof, scenarios);
	return cliUnreachableReasons(entry, proof);
}
