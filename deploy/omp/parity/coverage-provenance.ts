import type { InventoryEntry } from "./inventory-types";
import type { ProbeVisit } from "./probe";
import { absenceClass, nativeAbsenceReasons } from "./coverage-native-absence";

const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
interface Location { source: string; line: number }
const location = (value: unknown): value is Location => record(value) && typeof value.source === "string" && Number.isInteger(value.line);
// The browser always advertises this kind, so only a host without it reaches the source: proven from source.
const hostGated = (gate: unknown): boolean => record(gate) && gate.hostAdvertised === true && typeof gate.kind === "string" && location(gate) && location(gate.host);
const sameLocation = (visit: ProbeVisit, origin: Location) => visit.source === origin.source && visit.line === origin.line;

function locations(value: unknown, result: Location[] = []): Location[] {
  if (!value || typeof value !== "object") return result;
  if (location(value)) result.push(value);
  for (const child of Object.values(value)) locations(child, result);
  return result;
}

function componentReasons(entry: InventoryEntry, visits: readonly ProbeVisit[], scenarios: readonly (readonly ProbeVisit[])[]): string[] {
  const details = entry.details;
  const identity = entry.id.slice("component:".length);
  // An abstract class is never the runtime receiver; its hooks execute as its concrete subclasses.
  const receivers = [identity, ...(details.abstract === true && Array.isArray(details.concreteSubclasses) ? details.concreteSubclasses.filter((id): id is string => typeof id === "string").map(id => id.slice("component:".length)) : [])];
  const receives = (visit: ProbeVisit) => receivers.includes(String(visit.receiverComponent)) || visit.component === identity;
  if (!Array.isArray(details.hooks)) return ["Missing source hook provenance"];
  const reasons: string[] = [];
  // A class proof replaces the native-hook obligations it proves unreachable; cli-unreachable never runs in either leg, and a native transport never runs on the terminal leg.
  const absence = absenceClass(details);
  const terminalFree = absence === "cli-unreachable" || absence === "native-transport";
  const hooks = details.hooks.filter(record);
  const gatedHooks = new Set(details.hooks.filter(hook => record(hook) && hostGated(hook.kindGate)).map(hook => String((hook as Record<string, unknown>).name)));
  const exercised = (hook: Record<string, unknown>) => location(hook.implementation) && visits.some(visit => sameLocation(visit, hook.implementation as unknown as Location)
    && visit.adapter === (/^(?:describe|nativeSheet|handleNativeEvent)/.test(String(hook.name)) ? "browser" : "terminal")
    && !["access", "feature"].includes(visit.method) && !visit.method.startsWith("return:")
    && (receivers.includes(String(visit.receiverComponent)) || !hook.inherited && visit.component === identity));
  const terminal = hooks.filter(hook => typeof hook.name === "string" && /^render/.test(hook.name));
  const native = hooks.filter(hook => typeof hook.name === "string" && (/^describe/.test(hook.name) || hook.name === "nativeSheet"));
  // A hook whose name no source ever reads has no caller, so no production path can exercise it.
  for (const hook of terminalFree ? [] : absence ? terminal : [...terminal, ...native]) if (hook.referenced !== false && !gatedHooks.has(String(hook.name)) && !exercised(hook)) reasons.push(`Unexercised production hook ${hook.name}`);
  if (!absence && native.some(hook => !gatedHooks.has(String(hook.name))) && !visits.some(visit => visit.adapter === "browser" && visit.method.startsWith("return:")
    && receives(visit)
    && record(visit.arguments?.[0]) && visit.arguments[0].result === "value")) reasons.push("No actual non-null native descriptor result");
  const ownerUsed = (key: string) => Array.isArray(details[key]) && details[key].some(use => {
    if (!record(use) || !location(use) || typeof use.owner !== "string") return false;
    const owner = use.owner.slice("component:".length);
    return visits.some(visit => visit.method === "access" && sameLocation(visit, use)
      && visit.adapter === (key === "nativeOwners" ? "browser" : "terminal")
      && (visit.component === owner || visit.receiverComponent === owner) && visit.expression === use.expression);
  });
  if (!terminalFree && absence !== "input-listener" && !terminal.length && !ownerUsed("terminalOwners")) reasons.push("No exercised terminal production owner");
  if (!absence && !native.length && !ownerUsed("nativeOwners")) reasons.push("No exercised native production owner");
  if (absence) reasons.push(...nativeAbsenceReasons(entry, scenarios));
  else if (["rows-fallback", "null-description", "native-hook-absent"].includes(String(details.nativeStatus))) reasons.push(`Unsupported native source state: ${details.nativeStatus}`);
  if (Array.isArray(details.obligations) && details.obligations.includes("unresolved-inherited-hooks")) reasons.push("Unresolved source inheritance cannot be waived");
  if (Array.isArray(details.delegatedHelpers)) for (const use of details.delegatedHelpers) {
    if (!record(use) || !location(use)) { reasons.push("Malformed delegated source helper"); continue; }
    if (hostGated(use.kindGate) || gatedHooks.has(String(use.hook))) continue;
    if (!visits.some(visit => visit.method === "access" && sameLocation(visit, use) && visit.expression === use.expression
      && typeof use.owner === "string" && (visit.component === use.owner.slice("component:".length) || visit.receiverComponent === use.owner.slice("component:".length))
      && visit.adapter === (absence === "native-transport" || /^(?:describe|nativeSheet)/.test(String(use.hook)) ? "browser" : "terminal"))) reasons.push(`Unexercised delegated helper ${use.expression}`);
  }
  if (Array.isArray(details.nullReturns)) for (const origin of details.nullReturns) {
    if (record(origin) && (hostGated(origin.kindGate) || gatedHooks.has(String(origin.hook)))) continue;
    if (!location(origin) || !visits.some(visit => visit.adapter === "browser" && visit.method.startsWith("return:") && sameLocation(visit, origin)
      && receives(visit)
      && record(visit.arguments?.[0]) && ["null", "undefined"].includes(String(visit.arguments[0].result)))) reasons.push("Unexercised conditional native empty-return path");
  }
  if (details.interactive === true && !terminalFree) {
    const input = hooks.filter(hook => typeof hook.name === "string" && /^(?:handleInput|handleNativeEvent|handleMouse|handleKey|onKey|onMouse)/.test(hook.name));
    for (const hook of input) if (!exercised(hook)) reasons.push(`Unexercised production input hook ${hook.name}`);
    if (!absence && !input.some(hook => hook.name === "handleNativeEvent") && !ownerUsed("nativeOwners")) reasons.push("Interactive component has no exercised native input owner");
  }
  return reasons;
}

const COMPOSER_CHROME = "packages/tui/src/components/composer/native-chrome.ts";

/**
 * Composer styles have no describe hook: native chrome calls their render hooks. The remote
 * terminal calls the same hooks too, so only a call nested inside one synchronous
 * `describeComposerChrome` call..return window on the browser leg is native execution.
 */
function insideComposerChrome(visit: ProbeVisit, visits: readonly ProbeVisit[]): boolean {
  const same = (other: ProbeVisit) => other.adapter === "browser" && other.pid === visit.pid && other.generation === visit.generation && other.source === COMPOSER_CHROME;
  const opened = visits.filter(other => same(other) && other.method === "describeComposerChrome" && other.sequence < visit.sequence)
    .reduce((latest, other) => Math.max(latest, other.sequence), -1);
  if (opened < 0) return false;
  const closed = visits.filter(other => same(other) && other.method === "return:describeComposerChrome" && other.sequence > opened)
    .reduce((first, other) => Math.min(first, other.sequence), Number.POSITIVE_INFINITY);
  return visit.sequence < closed && Number.isFinite(closed);
}

/**
 * A feature the browser's open action never lists answers false in the browser, so the
 * negotiated-feature evidence cannot exist; instead every check must have run in the
 * browser while the run's hello omitted the name and listed nothing the host did not advertise.
 * Undefined when the entry makes no such claim or the run did negotiate the feature.
 */
function unadvertisedFeatureReasons(entry: InventoryEntry, visits: readonly ProbeVisit[], wireFacts: ReadonlySet<string>): string[] | undefined {
  const host = entry.details.hostFeatures;
  const name = entry.details.name;
  if (!record(host) || !location(host) || !Array.isArray(host.advertised) || typeof name !== "string" || host.advertised.includes(name) || !Array.isArray(entry.details.checks)) return undefined;
  const negotiated = [...wireFacts].filter(fact => fact.startsWith("advertised-feature:")).map(fact => fact.slice("advertised-feature:".length));
  if (negotiated.includes(name)) return undefined;
  if (!negotiated.length) return ["No hello reply in the run shows what the host negotiated"];
  const advertised: unknown[] = host.advertised;
  if (negotiated.some(feature => !advertised.includes(feature))) return ["The run negotiated a feature the host's open action does not list"];
  const unrun = entry.details.checks.filter(check => !(location(check) && visits.some(visit => visit.adapter === "browser" && visit.method === "feature" && sameLocation(visit, check) && visit.arguments?.[0] === name)));
  return unrun.length ? [`No browser execution of the unadvertised-feature check ${unrun.map(check => location(check) ? `${check.source}:${check.line}` : "?").join(", ")}`] : [];
}
/** A declaration location or a covers label alone never establishes execution. */
export function provenanceReasons(entry: InventoryEntry, visits: readonly ProbeVisit[], wireFacts: ReadonlySet<string>, scenarios: readonly (readonly ProbeVisit[])[] = [visits]): string[] {
  if (["kind", "property", "verb", "event", "operation", "query", "reply"].includes(entry.category)) return wireFacts.has(entry.id) ? [] : ["No matching source-schema-validated wire value"];
  if (entry.category === "component") return componentReasons(entry, visits, scenarios);
  if (entry.category === "feature" || entry.category === "feature-check") {
    const unadvertised = entry.category === "feature" ? unadvertisedFeatureReasons(entry, visits, wireFacts) : undefined;
    if (unadvertised) return unadvertised;
    const checks = entry.category === "feature" && Array.isArray(entry.details.checks) ? entry.details.checks : [entry];
    const matched = checks.some(check => location(check) && visits.some(visit => visit.adapter === "browser" && visit.method === "feature" && sameLocation(visit, check)
      && typeof visit.arguments?.[0] === "string" && wireFacts.has(`advertised-feature:${visit.arguments[0]}`)
      && (entry.category === "feature-check" || visit.arguments[0] === entry.details.name)));
    return matched ? [] : ["No negotiated feature and executed canonical check with its actual name"];
  }
  if (entry.category === "command" || entry.category === "subcommand") {
    if (!record(entry.details.handlers)) return ["Missing canonical command handler provenance"];
    const handlerLocations = locations(entry.details.handlers);
    const token = entry.category === "subcommand" ? entry.details.token : entry.id.slice("command:".length);
    const invoked = ["terminal", "browser"].every(adapter => visits.some(visit => visit.adapter === adapter && handlerLocations.some(origin => sameLocation(visit, origin))
      && !["access", "feature"].includes(visit.method) && !visit.method.startsWith("return:")
      && record(visit.arguments?.[0]) && visit.arguments[0].name === token
      && (entry.category !== "subcommand" || typeof visit.arguments[0].args === "string" && visit.arguments[0].args.trim().split(/\s+/u)[0] === entry.details.name)));
    return invoked ? [] : ["No invocation of the actual command/subcommand/alias handler with its parsed arguments on both adapters"];
  }
  if (entry.category === "setting") {
    const id = entry.id.slice("setting:".length);
    const wrote = ["terminal", "browser"].every(adapter => visits.some(visit => visit.adapter === adapter && visit.source === "packages/coding-agent/src/config/settings.ts" && visit.method === "writeValue"
      && record(visit.arguments?.[0]) && visit.arguments[0].setting === id));
    return (entry.details.configOnly === true ? wrote : wireFacts.has(entry.id)) ? [] : [entry.details.configOnly === true ? "No successful canonical config-only setting write" : "No actual settings row with this stable setting ID"];
  }
  if (["setting-choice", "setting-condition", "setting-options-factory", "setting-options-state", "settings-tab", "control"].includes(entry.category)) {
    return wireFacts.has(entry.id) ? [] : ["No explicit observable behavior with its actual canonical settings/control owner"];
  }
  if (["tool", "composer", "segment", "scene"].includes(entry.category)) {
    const origins = locations(entry.details);
    const relevant = visits.filter(visit => origins.some(origin => sameLocation(visit, origin) || visit.source === origin.source && visit.ownerLine === origin.line));
    const terminal = relevant.some(visit => visit.adapter === "terminal" && /^render/.test(visit.method));
    const native = relevant.some(visit => visit.adapter === "browser" && (/^describe/.test(visit.method) || visit.method === "nativeSheet"
      || entry.category === "composer" && /^render/.test(visit.method) && insideComposerChrome(visit, visits)));
    return [...(!terminal ? ["No actual registry terminal renderer execution"] : []), ...(!native ? ["No actual registry native descriptor execution"] : [])];
  }
  return [`Unsupported inventory category ${entry.category}`];
}
