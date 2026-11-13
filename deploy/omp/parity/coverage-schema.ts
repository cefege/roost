import type { RuntimeSchema, SchemaNode } from "./inventory-types";
import type { ProbeDocument, WireObservation } from "./probe";
import { SchemaResolution, UnsupportedSchema, type SchemaBinding } from "./coverage-schema-resolution";

export interface PropertyWitness { entry: string; value: unknown; pointer: string; surface?: string; node?: string; kind?: string }
export interface SchemaObservation { facts: Set<string>; values: PropertyWitness[] }
interface Match extends SchemaObservation { valid: boolean; unsupported: string[] }
interface Context { pointer: string; surface?: string; node?: string; kind?: string }
const invalid = (): Match => ({ valid: false, facts: new Set(), values: [], unsupported: [] });
const valid = (facts: Iterable<string> = [], values: PropertyWitness[] = []): Match => ({ valid: true, facts: new Set(facts), values, unsupported: [] });
const objectValue = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const owns = (value: object, name: string) => Object.prototype.hasOwnProperty.call(value, name);

/** Reads actual decoded wire values against the source graph, never a parallel document fold. */
export class WireSchemaEvidence {
  private readonly resolution: SchemaResolution;
  constructor(graph: RuntimeSchema) { this.resolution = new SchemaResolution(graph); }

  observe(row: WireObservation): Set<string> { return this.inspect(row).facts; }

  inspect(row: WireObservation): SchemaObservation {
    const roots: Record<string, string> = { o: "TspOpen", f: "TspFrame", x: "TspClose", q: "TspQuery", r: "TspReply", e: "TspEvent", t: "TspPalette" };
    if (row.verb === "b") {
      if (row.direction !== "out" || typeof row.body !== "string") throw new Error("Malformed observed blob transport");
      return valid(["verb:b"]);
    }
    const root = roots[row.verb];
    if (!root || (row.direction === "in") !== ["r", "e"].includes(row.verb)) throw new Error(`Unsupported observed wire direction/verb ${row.direction}/${row.verb}`);
    const result = this.match(this.resolution.binding(root), row.body, 0, {
      pointer: "", ...(row.verb === "f" && objectValue(row.body) && typeof row.body.sf === "string" ? { surface: row.body.sf } : {}),
    });
    if (!result.valid) throw new Error(`Observed ${root} does not satisfy its retained source schema${result.unsupported.length ? `: ${result.unsupported.join("; ")}` : ""}`);
    result.facts.add(`verb:${row.verb}`);
    return result;
  }

  document(row: ProbeDocument): SchemaObservation {
    const result = this.match(this.resolution.binding("TspNode"), row.document, 0, { pointer: "", surface: row.surface });
    if (!result.valid) throw new Error(`Observed canonical document does not satisfy TspNode: ${result.unsupported.join("; ")}`);
    return result;
  }

  private match(input: SchemaBinding, value: unknown, depth: number, context: Context): Match {
    if (depth > 256) return { ...invalid(), unsupported: ["Observed schema/value depth exceeded its bound"] };
    try {
      const name = input.type.kind === "ref" ? input.type.name : undefined;
      if (name === "TspNode" && objectValue(value) && typeof value.id === "string" && typeof value.k === "string") context = { ...context, node: value.id, kind: value.k };
      const binding = this.resolution.expand(input);
      const result = this.matchExpanded(binding, value, depth, context);
      if (!result.valid) return result;
      for (const claim of new Set([...(input.claims ?? []), ...(binding.claims ?? [])])) {
        result.facts.add(claim);
        result.values.push({ entry: claim, value, ...context });
      }
      if ((name === "TspNode" || name === "TspPropNode") && objectValue(value) && typeof value.k === "string") {
        result.facts.add(`kind:${value.k}`);
        result.values.push({ entry: `kind:${value.k}`, value, ...context });
      }
      if (name === "TspOp" && Array.isArray(value) && typeof value[0] === "string") result.facts.add(`operation:${value[0]}`);
      if (name === "TspPrefsRow" && objectValue(value) && typeof value.id === "string") {
        result.facts.add(`setting:${value.id}`);
        result.values.push({ entry: `setting:${value.id}`, value, ...context });
      }
      if (name === "TspReply" && objectValue(value) && value.r === "hello" && Array.isArray(value.features)) {
        for (const feature of value.features) if (typeof feature === "string") result.facts.add(`advertised-feature:${feature}`);
      }
      for (const [type, category, key] of [["TspEvent", "event", "ev"], ["TspQuery", "query", "q"], ["TspReply", "reply", "r"]]) {
        if (name === type && objectValue(value) && typeof value[key!] === "string") result.facts.add(`${category}:${value[key!]}`);
      }
      return result;
    } catch (error) {
      if (!(error instanceof UnsupportedSchema)) throw error;
      return { ...invalid(), unsupported: [error.message] };
    }
  }

  private matchExpanded(binding: SchemaBinding, value: unknown, depth: number, context: Context): Match {
    const node = binding.type;
    const nested = (key: string | number): Context => ({ ...context, pointer: `${context.pointer}/${String(key).replaceAll("~", "~0").replaceAll("/", "~1")}` });
    const child = (type: SchemaNode, item: unknown, claims?: readonly string[], key?: string | number) => this.match({ type, scope: binding.scope, claims }, item, depth + 1, key === undefined ? context : nested(key));
    if (node.kind === "literal") return Object.is(value, node.value) ? valid() : invalid();
    if (node.kind === "scalar") {
      const accepts = node.name === "any" || node.name === "unknown"
        || node.name === "undefined" && value === undefined || node.name === "void" && value === undefined
        || ["string", "boolean"].includes(node.name) && typeof value === node.name
        || node.name === "number" && typeof value === "number" && Number.isFinite(value)
        || node.name === "object" && value !== null && typeof value === "object";
      if (!["any", "unknown", "undefined", "void", "string", "boolean", "number", "object", "never"].includes(node.name)) throw new UnsupportedSchema(`Unsupported scalar ${node.name}`);
      return accepts ? valid() : invalid();
    }
    if (node.kind === "union") return this.alternatives(node.types.map(type => () => child(type, value)));
    if (node.kind === "intersection") return this.requireAll(node.types.map(type => () => this.match({ ...binding, type }, value, depth + 1, context)));
    if (node.kind === "indexed") return this.alternatives(this.resolution.indexed(binding).map(member => () => this.match(member, value, depth + 1, context)));
    if (node.kind === "array") return Array.isArray(value) ? this.requireAll(value.map((item, index) => () => child(node.item, item, undefined, index))) : invalid();
    if (node.kind === "tuple") {
      if (!Array.isArray(value)) return invalid();
      const rest = node.items.findIndex(item => item.rest);
      if (rest >= 0 && rest !== node.items.length - 1) throw new UnsupportedSchema("Nonfinal tuple rest");
      const required = node.items.filter(item => !item.optional && !item.rest).length;
      if (value.length < required || rest < 0 && value.length > node.items.length) return invalid();
      const checks: Array<() => Match> = [];
      for (const [index, item] of node.items.entries()) {
        if (item.rest) checks.push(() => child(item.type, value.slice(index), item.propertyIds, index));
        else if (index < value.length) checks.push(() => child(item.type, value[index], item.propertyIds, index));
      }
      return this.requireAll(checks);
    }
    if (node.kind === "object") {
      if (node.fields.length === 0 && node.indexes.length === 0) return value !== null && value !== undefined ? valid() : invalid();
      const included = (name: string) => !binding.omit?.has(name) && (!binding.pick || binding.pick.has(name));
      if (!objectValue(value)) return invalid();
      const checks: Array<() => Match> = [];
      for (const field of node.fields) {
        if (!included(field.name)) continue;
        if (!owns(value, field.name)) {
          if (!(binding.optional ?? field.optional)) return invalid();
        } else checks.push(() => child(field.type, value[field.name], field.propertyIds, field.name));
      }
      for (const index of node.indexes) for (const [key, item] of Object.entries(value)) {
        if (!included(key)) continue;
        checks.push(() => child(index.key, key));
        checks.push(() => child(index.value, item, index.propertyIds, key));
      }
      return this.requireAll(checks);
    }
    if (node.kind === "mapped") {
      if (!objectValue(value)) return invalid();
      const checks: Array<() => Match> = [];
      for (const member of this.resolution.mapped(binding)) {
        if (binding.omit?.has(member.key) || binding.pick && !binding.pick.has(member.key)) continue;
        if (!owns(value, member.key)) {
          if (!(binding.optional ?? ["?", "+?"].includes(node.optional))) return invalid();
        } else checks.push(() => this.match(member.value, value[member.key], depth + 1, nested(member.key)));
      }
      return this.requireAll(checks);
    }
    if (node.kind === "template") {
      if (typeof value !== "string") return invalid();
      const escape = (text: string) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      let expression = escape(node.head);
      for (const span of node.spans) {
        const member = this.resolution.expand({ type: span.type, scope: binding.scope });
        const values = this.resolution.literals(member);
        if (values) expression += `(?:${values.map(item => escape(String(item))).join("|")})`;
        else if (member.type.kind === "scalar" && member.type.name === "number") expression += "[-+]?(?:\\d+(?:\\.\\d*)?|\\.\\d+)(?:[eE][-+]?\\d+)?";
        else if (member.type.kind === "scalar" && member.type.name === "string") expression += "[\\s\\S]*";
        else throw new UnsupportedSchema(`Unsupported template span at ${span.type.source}:${span.type.line}`);
        expression += escape(span.text);
      }
      return new RegExp(`^(?:${expression})$`, "u").test(value) ? valid() : invalid();
    }
    throw new UnsupportedSchema(`Unsupported ${node.kind} at ${node.source}:${node.line}${node.kind === "unknown" ? ` (${node.expression})` : ""}`);
  }

  private alternatives(checks: Array<() => Match>): Match {
    const results = checks.map(check => check());
    const matches = results.filter(result => result.valid);
    return matches.length ? valid(matches.flatMap(result => [...result.facts]), matches.flatMap(result => result.values)) : { ...invalid(), unsupported: [...new Set(results.flatMap(result => result.unsupported))] };
  }

  private requireAll(checks: Array<() => Match>): Match {
    const combined = valid();
    for (const check of checks) {
      const result = check();
      if (!result.valid) return result;
      for (const fact of result.facts) combined.facts.add(fact);
      combined.values.push(...result.values);
    }
    return combined;
  }
}
