import type { BehaviorClaim } from "./scenario";
import type { InventoryEntry } from "./inventory-types";
import type { BehaviorProof } from "./coverage-behavior";
import { equivalent } from "./coverage";
import { assertions, currentVisits, dispatchedInput, location, record, requireVisible, sameLocation, type BehaviorContext, type LocatedBehavior, type SourceValue } from "./coverage-source-value";

const categories = new Set(["setting", "control", "setting-choice", "setting-condition", "setting-options-factory", "setting-options-state"]);
const result = (comparison: LocatedBehavior, value: unknown) => ["effect", "file"].includes(comparison.comparison.terminal.type)
  && ["effect", "file"].includes(comparison.comparison.browser.type) && equivalent(comparison.comparison.terminalValue, value) && equivalent(comparison.comparison.browserValue, value);

function settingsSource(context: BehaviorContext, actual: SourceValue, producer: unknown) {
  if (!location(producer)) throw new Error("Settings proof lacks its actual source projection producer");
  const terminal = currentVisits(context.scenario, context.target, "terminal"), browser = currentVisits(context.scenario, context.target, "browser", actual.row.sequence);
  const component = [...context.entries.values()].find(entry => entry.category === "component" && entry.source === producer.source
    && Array.isArray(entry.details.hooks) && entry.details.hooks.some(hook => record(hook) && hook.name === "describe"));
  const hooks = component && Array.isArray(component.details.hooks) ? component.details.hooks.filter(record) : [];
  const terminalVisit = terminal.findLast(visit => /^render/.test(visit.method) && hooks.some(hook => /^render/.test(String(hook.name)) && location(hook.implementation) && sameLocation(visit, hook.implementation)));
  const native = browser.some(visit => hooks.some(hook => /^describe/.test(String(hook.name)) && location(hook.implementation) && sameLocation(visit, hook.implementation)));
  if (!terminalVisit || !native || !browser.some(visit => sameLocation(visit, producer))) throw new Error("Settings proof lacks actual same-generation TUI rendering and native projection execution");
  return terminalVisit;
}

function persistence(context: BehaviorContext, groups: LocatedBehavior[], setting: string, nativeValue: unknown, projection: unknown): void {
  if (!dispatchedInput(context)) throw new Error(`Setting ${setting} requires actual dispatched input on both adapters`);
  const current = groups.filter(group => group.index === context.current.index);
  const callbacks = current.filter(group => group.comparison.terminal.type === "effect" && group.comparison.browser.type === "effect"
    && group.comparison.terminal.channel === `change:${setting}` && group.comparison.browser.channel === `change:${setting}`);
  const saved = current.filter(group => group.comparison.terminal.type === "effect" && group.comparison.browser.type === "effect"
    && group.comparison.terminal.channel === `saved:${setting}` && group.comparison.browser.channel === `saved:${setting}`);
  if (!callbacks.length || !saved.some(group => result(group, nativeValue))) throw new Error(`Setting ${setting} requires exact canonical callback and independently reloaded persisted value`);
  const callbackValue = callbacks.find(group => equivalent(group.comparison.terminalValue, group.comparison.browserValue))?.comparison.terminalValue;
  const numeric = record(projection) && location(projection.converter) && typeof nativeValue === "number";
  if (!equivalent(callbackValue, nativeValue) && !(numeric && typeof callbackValue === "string" && callbackValue.trim() && Number(callbackValue) === nativeValue)) throw new Error(`Setting ${setting} callback is not its real source-converted persisted value`);
  for (const adapter of ["terminal", "browser"] as const) if (!currentVisits(context.scenario, context.current, adapter).some(visit => visit.method === "writeValue"
    && visit.source === "packages/coding-agent/src/config/settings.ts" && record(visit.arguments?.[0]) && visit.arguments[0].setting === setting)) throw new Error(`Setting ${setting} lacks a successful canonical ${adapter} write`);
}

/** Settings claims are whole typed rows plus real source projections, choices and persistence. */
export function settingsBehaviorProof(entry: InventoryEntry, claim: BehaviorClaim, context: BehaviorContext): BehaviorProof | undefined {
  if (!categories.has(entry.category)) return undefined;
  const groups = assertions(context, claim.assertionGroups);
  requireVisible(context.target);
  const snapshot = context.values(context.target);
  const identity = context.target.comparison.browserTarget?.identity;
  const setting = typeof entry.details.settingId === "string" ? entry.details.settingId : entry.category === "setting" ? entry.id.slice("setting:".length) : identity?.setting;
  if (!setting) throw new Error(`Settings ${entry.id} has no canonical actual setting row identity`);
  const definition = context.entries.get(`setting:${setting}`);
  if (!definition || entry.category === "control" && (!Array.isArray(entry.details.settings) || !entry.details.settings.includes(setting))) throw new Error(`Settings ${entry.id} does not own the observed row`);
  const projection = record(entry.details.nativeProjection) ? entry.details.nativeProjection : definition.details.nativeProjection;
  const producer = record(projection) ? projection.producer : entry.details.producer;
  const condition = entry.category === "setting-condition";
  const outcome = entry.details.outcome;
  let actual = snapshot.values.find(witness => witness.entry === `setting:${setting}` && record(witness.value));
  if (condition) {
    if (claim.value !== outcome || typeof outcome !== "boolean") throw new Error("Condition claim must be its actual source boolean outcome");
    const presentation = entry.details.presentation;
    const gate = record(presentation) ? presentation.gate : undefined;
    if (!record(presentation) || !record(gate) || !location(gate) || !record(gate.owner)) throw new Error("Condition has no actual row-building presentation gate");
    for (const adapter of ["terminal", "browser"] as const) {
      const cursor = context.target.checkpoint.probeCursors?.[adapter];
      if (!cursor || !(context.scenario.raw ?? []).some(row => row.type === "condition" && row.adapter === adapter && row.pid === cursor.pid
        && row.generation === cursor.generation && Number(row.sequence) <= cursor.sequence && row.setting === setting && row.outcome === outcome
        && row.source === gate.source && row.line === gate.line)) throw new Error(`Condition lacks actual ${adapter} row-gate evaluation for ${setting}`);
    }
    if (outcome) {
      if (!actual || identity?.setting !== setting) throw new Error("Visible condition requires its actual current typed row and DOM identity");
    } else {
      const prefs = context.target.comparison.browserTarget?.prefs;
      const tab = typeof presentation.tab === "string" ? presentation.tab : definition.details.tab;
      const absent = groups.some(group => group.index === context.current.index && group.comparison.terminal.type === "screen" && group.comparison.terminal.read === "visible"
        && group.comparison.terminalValue === false && group.comparison.browser.type === "dom" && group.comparison.browser.read === "count" && group.comparison.browserValue === 0
        && group.comparison.browser.selector.includes(`[data-omp-setting=${JSON.stringify(setting)}]`));
      if (actual || !prefs || prefs.rows.includes(setting) || !tab || prefs.tab !== tab && prefs.page !== tab || !absent) throw new Error("Hidden condition requires matching current prefs page, DOM row absence and independent current TTY row absence");
      actual = snapshot.values.find(witness => witness.entry === "kind:prefs");
      if (!actual) throw new Error("Hidden condition requires a separate current visible typed prefs owner");
    }
  } else if (!actual || identity?.setting !== setting) throw new Error("Settings proof target must be its actual typed row, not an unrelated title");
  const sourceValue: SourceValue = { row: snapshot.row, witness: actual! };
  const terminal = settingsSource(context, sourceValue, producer);
  const row = actual!.value;
  const control = record(row) && record(row.control) ? row.control : undefined;
  let value: unknown = claim.value;
  if (!condition) {
    if (!control || !record(projection) || !Array.isArray(projection.returns) || !projection.returns.some(returned => record(returned) && returned.kind === control.k)) throw new Error("Typed settings row does not match an actual source control projection");
    const options = Array.isArray(control.options) ? control.options : undefined;
    if (entry.category === "setting-options-factory" || entry.category === "setting-options-state") {
      const state = entry.category === "setting-options-state" ? entry : (Array.isArray(entry.details.states) ? entry.details.states : []).map(id => context.entries.get(String(id))).find(state => state && equivalent(state.details.value, claim.value));
      const actualValues = options?.map(option => record(option) ? option.value : undefined);
      if (!state || !Array.isArray(claim.value) || !equivalent(actualValues, claim.value) || !equivalent(state.details.value, claim.value)) throw new Error("Options proof requires the entire actual current row.control.options value array and its real source state");
      if (state.details.model && !groups.some(group => result(group, state.details.model))) throw new Error("Model-dependent options proof requires its actual modeled runtime context on both adapters");
      for (const option of options ?? []) if (!record(option) || !groups.some(group => group.comparison.terminal.type === "screen" && group.comparison.browser.type === "dom"
        && equivalent(group.comparison.terminalValue, option.label) && equivalent(group.comparison.browserValue, option.label)
        && group.comparison.browserTarget?.identity?.setting === setting)) throw new Error("Every runtime option needs its own real TTY/DOM choice observation");
      persistence(context, groups, setting, controlValue(control), projection);
      value = actualValues;
    } else {
      const currentValue = controlValue(control);
      if (entry.category === "setting-choice") {
        const candidates = Array.isArray(entry.details.nativeValues) ? entry.details.nativeValues : [];
        if (!candidates.some(candidate => record(candidate) && candidate.kind === control.k && (equivalent(candidate.value, currentValue)
          || Array.isArray(currentValue) && currentValue.some(value => equivalent(value, candidate.value))))) throw new Error("Choice proof requires its actual source-converted native value");
      }
      if (control.secret === true && context.target.comparison.browserTarget?.state.mask !== true) throw new Error("Masked settings require an actual DOM password boolean, not bullet text");
      if (!equivalent(currentValue, claim.value)) throw new Error("Settings claim differs from the current typed native control value");
      persistence(context, groups, setting, currentValue, projection);
      value = currentValue;
    }
  }
  return { entry: entry.id, value, effect: claim.effect, checkpoint: context.current.checkpoint.name, comparison: context.current.comparison.name,
    source: { pid: snapshot.row.pid, generation: snapshot.row.generation!, sequence: snapshot.row.sequence, pointer: actual!.pointer },
    terminalSource: { pid: terminal.pid, generation: terminal.generation!, sequence: terminal.sequence, source: terminal.source, line: terminal.line },
    target: { surface: context.target.comparison.browserTarget!.surface!, node: actual!.node! } };
}

/** TspPrefsControl keeps its current value in a per-variant field (`on`, `value`, `values`, `keys`). */
function controlValue(control: Record<string, unknown>): unknown {
  if (control.k === "switch") return control.on;
  if (control.k === "multi") return control.values;
  if (control.k === "keys") return control.keys;
  return control.value;
}
