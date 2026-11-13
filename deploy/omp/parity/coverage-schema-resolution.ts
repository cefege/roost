import type { RuntimeSchema, SchemaNode } from "./inventory-types";

export interface SchemaBinding {
  type: SchemaNode;
  scope: Map<string, SchemaBinding>;
  claims?: readonly string[];
  optional?: boolean;
  omit?: ReadonlySet<string>;
  pick?: ReadonlySet<string>;
}
export class UnsupportedSchema extends Error {}

/** Resolves the retained TypeScript graph; it does not maintain a TSP document or apply operations. */
export class SchemaResolution {
  constructor(readonly graph: RuntimeSchema) {}

  binding(name: string): SchemaBinding {
    const declaration = this.graph[name];
    if (!declaration) throw new UnsupportedSchema(`Missing source schema ${name}`);
    return { type: { kind: "ref", name, arguments: [], source: declaration.source, line: declaration.line }, scope: new Map() };
  }

  expand(binding: SchemaBinding, depth = 0): SchemaBinding {
    if (depth > 128) throw new UnsupportedSchema("Recursive schema expansion exceeded its bound");
    const node = binding.type;
    if (node.kind === "ref") {
      const parameter = binding.scope.get(node.name);
      if (parameter) return this.expand({ ...parameter, ...this.policies(binding) }, depth + 1);
      if (["Readonly", "Partial", "Required", "Pick", "Omit"].includes(node.name)) {
        const target = node.arguments[0];
        if (!target) throw new UnsupportedSchema(`Missing ${node.name} argument`);
        let next: SchemaBinding = { ...binding, type: target };
        if (node.name === "Partial") next.optional = true;
        if (node.name === "Required") next.optional = false;
        if (node.name === "Pick" || node.name === "Omit") {
          const keys = node.arguments[1] && this.literals({ type: node.arguments[1], scope: binding.scope });
          if (!keys?.every(key => typeof key === "string")) throw new UnsupportedSchema(`Nonfinite ${node.name} keys`);
          const selected = new Set(keys as string[]);
          if (node.name === "Pick") next.pick = new Set([...selected].filter(key => !binding.pick || binding.pick.has(key)));
          else next.omit = new Set([...(binding.omit ?? []), ...selected]);
        }
        return this.expand(next, depth + 1);
      }
      if (["Array", "ReadonlyArray"].includes(node.name) && node.arguments[0]) {
        return { ...binding, type: { ...node, kind: "array", item: node.arguments[0] } };
      }
      if (node.name === "Record" && node.arguments[0] && node.arguments[1]) {
        // The inventory names a Record member `<owner>[key]`; each actual key's value claims it.
        const propertyIds = (binding.claims ?? []).map(claim => `${claim}[key]`);
        return { ...binding, type: { ...node, kind: "object", fields: [], indexes: [{ key: node.arguments[0], value: node.arguments[1], propertyIds }] } };
      }
      const declaration = this.graph[node.name];
      if (!declaration) throw new UnsupportedSchema(`Unresolved source type ${node.name} at ${node.source}:${node.line}`);
      const scope = new Map(binding.scope);
      for (const [index, parameter] of declaration.parameters.entries()) {
        const argument = node.arguments[index];
        const fallback = parameter.default ?? parameter.constraint;
        if (!argument && !fallback) throw new UnsupportedSchema(`Unbound ${node.name}.${parameter.name}`);
        scope.set(parameter.name, { type: argument ?? fallback!, scope: argument ? binding.scope : scope });
      }
      const type = declaration.extends.length
        ? { ...declaration.definition, kind: "intersection" as const, types: [...declaration.extends, declaration.definition] }
        : declaration.definition;
      return this.expand({ ...binding, type, scope }, depth + 1);
    }
    if (node.kind === "operator" && ["readonly", "optional", "rest"].includes(node.operator)) {
      return this.expand({ ...binding, type: node.type }, depth + 1);
    }
    if (node.kind === "conditional") {
      const left = this.literals({ type: node.check, scope: binding.scope });
      const right = this.literals({ type: node.extends, scope: binding.scope });
      if (!left || !right) throw new UnsupportedSchema(`Unresolved conditional at ${node.source}:${node.line}`);
      const matches = left.every(value => right.some(other => JSON.stringify(other) === JSON.stringify(value)));
      return this.expand({ ...binding, type: matches ? node.whenTrue : node.whenFalse }, depth + 1);
    }
    if (node.kind === "value") return this.expand({ ...binding, type: this.valueType(node.value, node) }, depth + 1);
    return binding;
  }

  literals(input: SchemaBinding, depth = 0): unknown[] | undefined {
    if (depth > 128) throw new UnsupportedSchema("Recursive literal type exceeded its bound");
    const binding = this.expand(input);
    const node = binding.type;
    if (node.kind === "literal") return [node.value];
    if (node.kind === "scalar" && node.name === "never") return [];
    if (node.kind === "union") {
      const members = node.types.map(type => this.literals({ type, scope: binding.scope }, depth + 1));
      return members.some(member => member === undefined) ? undefined : members.flat() as unknown[];
    }
    if (node.kind === "indexed") {
      const members = this.indexed(binding).map(member => this.literals(member, depth + 1));
      return members.some(member => member === undefined) ? undefined : members.flat() as unknown[];
    }
    if (node.kind === "tuple" && node.items.every(item => !item.optional && !item.rest)) {
      let tuples: unknown[][] = [[]];
      for (const item of node.items) {
        const values = this.literals({ type: item.type, scope: binding.scope }, depth + 1);
        if (!values) return undefined;
        tuples = tuples.flatMap(tuple => values.map(value => [...tuple, value]));
        if (tuples.length > 10000) throw new UnsupportedSchema("Unbounded literal tuple expansion");
      }
      return tuples;
    }
    if (node.kind === "operator" && node.operator === "keyof") return this.keys({ type: node.type, scope: binding.scope });
    return undefined;
  }

  indexed(binding: SchemaBinding): SchemaBinding[] {
    const node = binding.type;
    if (node.kind !== "indexed") throw new UnsupportedSchema("Expected indexed schema");
    const requested = this.literals({ type: node.index, scope: binding.scope });
    return this.project({ type: node.object, scope: binding.scope }, requested).map(member => ({
      ...member, ...this.policies(binding), claims: [...(binding.claims ?? []), ...(member.claims ?? [])],
    }));
  }

  mapped(binding: SchemaBinding): Array<{ key: string; value: SchemaBinding }> {
    const node = binding.type;
    if (node.kind !== "mapped") throw new UnsupportedSchema("Expected mapped schema");
    if (node.nameType) throw new UnsupportedSchema(`Mapped key remapping at ${node.source}:${node.line}`);
    const keys = this.literals({ type: node.constraint, scope: binding.scope });
    if (!keys || !keys.every(key => typeof key === "string" || typeof key === "number")) throw new UnsupportedSchema(`Nonfinite mapped schema at ${node.source}:${node.line}`);
    return keys.map(key => {
      const scope = new Map(binding.scope);
      scope.set(node.parameter, { type: { ...node, kind: "literal", value: key }, scope: binding.scope });
      return { key: String(key), value: { type: node.value, scope, claims: node.propertyIds } };
    });
  }

  private keys(input: SchemaBinding): string[] | undefined {
    const binding = this.expand(input);
    const node = binding.type;
    if (node.kind === "object") return node.indexes.length ? undefined : node.fields.map(field => field.name);
    if (node.kind === "mapped") return this.mapped(binding).map(member => member.key);
    if (node.kind === "intersection") {
      const groups = node.types.map(type => this.keys({ type, scope: binding.scope }));
      return groups.some(group => !group) ? undefined : [...new Set(groups.flat() as string[])];
    }
    if (node.kind === "indexed") {
      // `keyof (A | B)` is the keys every member shares.
      const groups = this.indexed(binding).map(member => this.keys(member));
      if (!groups.length || groups.some(group => !group)) return undefined;
      return (groups as string[][]).reduce((shared, group) => shared.filter(key => group.includes(key)));
    }
    throw new UnsupportedSchema(`Unsupported keyof target at ${node.source}:${node.line}`);
  }

  private project(input: SchemaBinding, requested: unknown[] | undefined): SchemaBinding[] {
    const binding = this.expand(input);
    const node = binding.type;
    if (node.kind === "object") {
      const fields = node.fields.filter(field => !requested || requested.includes(field.name));
      const result = fields.map(field => ({ type: field.type, scope: binding.scope, claims: field.propertyIds }));
      if (node.indexes.length) throw new UnsupportedSchema(`Indexed index signature at ${node.source}:${node.line}`);
      return result;
    }
    if (node.kind === "mapped") return this.mapped(binding).filter(member => !requested || requested.map(String).includes(member.key)).map(member => member.value);
    if (node.kind === "array") return [{ type: node.item, scope: binding.scope }];
    if (node.kind === "tuple") return node.items.filter((_, index) => !requested || requested.includes(index) || requested.includes(String(index))).map(item => ({ type: item.type, scope: binding.scope, claims: item.propertyIds }));
    if (node.kind === "union") return node.types.flatMap(type => this.project({ type, scope: binding.scope }, requested));
    throw new UnsupportedSchema(`Unsupported indexed target at ${node.source}:${node.line}`);
  }

  private policies(binding: SchemaBinding): Partial<SchemaBinding> {
    return Object.fromEntries(["claims", "optional", "omit", "pick"].filter(key => binding[key as keyof SchemaBinding] !== undefined).map(key => [key, binding[key as keyof SchemaBinding]]));
  }

  private valueType(value: unknown, origin: SchemaNode): SchemaNode {
    if (Array.isArray(value)) return { ...origin, kind: "tuple", items: value.map(item => ({ optional: false, rest: false, propertyIds: [], type: this.valueType(item, origin) })) };
    if (value !== null && typeof value === "object") return { ...origin, kind: "object", fields: Object.entries(value).map(([name, item]) => ({ name, optional: false, readonly: true, propertyIds: [], source: origin.source, line: origin.line, type: this.valueType(item, origin) })), indexes: [] };
    return { ...origin, kind: "literal", value };
  }
}
