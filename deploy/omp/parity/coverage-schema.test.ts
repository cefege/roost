import { describe, expect, test } from "bun:test";
import type { RuntimeSchema, SchemaNode } from "./inventory-types";
import type { WireObservation } from "./probe";
import { WireSchemaEvidence } from "./coverage-schema";

const origin = { source: "packages/wire/src/tsp.ts", line: 1 };
const scalar = (name: string): SchemaNode => ({ ...origin, kind: "scalar", name });
const literal = (value: unknown): SchemaNode => ({ ...origin, kind: "literal", value });
const reference = (name: string, ...args: SchemaNode[]): SchemaNode => ({ ...origin, kind: "ref", name, arguments: args });
const object = (fields: Array<[string, SchemaNode, string?, boolean?]>): SchemaNode => ({ ...origin, kind: "object", indexes: [], fields: fields.map(([name, type, propertyId, optional]) => ({ ...origin, name, type, optional: optional ?? false, readonly: false, propertyIds: propertyId ? [propertyId] : [] })) });
const tuple = (...types: SchemaNode[]): SchemaNode => ({ ...origin, kind: "tuple", items: types.map(type => ({ type, optional: false, rest: false, propertyIds: [] })) });
const alias = (definition: SchemaNode): RuntimeSchema[string] => ({ ...origin, kind: "alias", parameters: [], extends: [], definition });

function graph(): RuntimeSchema {
  return {
    TspKind: alias({ ...origin, kind: "union", types: [literal("button"), literal("json"), literal("chart")] }),
    TspCommonProps: alias(object([["min", scalar("string"), "property:TspCommonProps.min", true], ["actions", object([["click", scalar("string"), "property:TspCommonProps.actions.click", true]]), "property:TspCommonProps.actions", true]])),
    TspButtonProps: alias(object([["disabled", scalar("boolean"), "property:TspButtonProps.disabled", true], ["actions", { ...origin, kind: "array", item: scalar("string") }, "property:TspButtonProps.actions", true]])),
    TspJsonProps: alias(object([["value", scalar("unknown"), "property:TspJsonProps.value"]])),
    TspChartProps: alias(object([["min", scalar("number"), "property:TspChartProps.min", true]])),
    TspPropsByKind: alias(object([["button", reference("TspButtonProps")], ["json", reference("TspJsonProps")], ["chart", reference("TspChartProps")]])),
    TspProps: { ...alias({ ...origin, kind: "conditional", check: reference("K"), extends: reference("TspKind"),
      whenTrue: { ...origin, kind: "intersection", types: [
        { ...origin, kind: "indexed", object: reference("TspPropsByKind"), index: reference("K") },
        reference("Omit", reference("TspCommonProps"), { ...origin, kind: "operator", operator: "keyof", type: { ...origin, kind: "indexed", object: reference("TspPropsByKind"), index: reference("K") } }),
      ] },
      whenFalse: scalar("never") }),
      parameters: [{ name: "K", constraint: reference("TspKind") }] },
    TspNode: alias({ ...origin, kind: "indexed", index: reference("TspKind"), object: { ...origin, kind: "mapped", parameter: "K", constraint: reference("TspKind"), optional: "preserve", readonly: "preserve", propertyIds: [], value: object([
      ["id", scalar("string")], ["k", reference("K")], ["p", reference("TspProps", reference("K")), undefined, true],
    ]) } }),
    TspOp: alias({ ...origin, kind: "union", types: [tuple(literal("add"), scalar("string"), scalar("string"), literal(null), reference("TspNode")), tuple(literal("set"), scalar("string"), reference("Record", scalar("string"), scalar("unknown")))] }),
    TspFrame: alias(object([["sf", scalar("string")], ["s", scalar("number")], ["ops", { ...origin, kind: "array", item: reference("TspOp") }]])),
  };
}
function frame(node: unknown): WireObservation {
  return { type: "wire", adapter: "browser", generation: "unit-boundary", pid: 1, sequence: 1, source: "packages/tui/src/native/backend.ts", line: 1, direction: "out", verb: "f", body: { sf: "surface", s: 1, ops: [["add", "node", "root", null, node]] } };
}

describe("source-schema evidence ownership", () => {
  test("arbitrary JSON payloads cannot masquerade as native node/property evidence", () => {
    const facts = new WireSchemaEvidence(graph()).observe(frame({ id: "node", k: "json", p: { value: { k: "button", p: { disabled: true } } } }));
    expect(facts.has("kind:json")).toBe(true);
    expect(facts.has("property:TspJsonProps.value")).toBe(true);
    expect(facts.has("kind:button")).toBe(false);
    expect(facts.has("property:TspButtonProps.disabled")).toBe(false);
  });

  test("wrong-kind and wrong-typed properties cannot satisfy another branch", () => {
    expect(() => new WireSchemaEvidence(graph()).observe(frame({ id: "node", k: "button", p: { disabled: "yes" } }))).toThrow("does not satisfy");
    expect(() => new WireSchemaEvidence(graph()).observe(frame({ id: "node", k: "missing", p: {} }))).toThrow("does not satisfy");
  });

  test("a kind prop replaces the same-named common prop and keeps its actual owner", () => {
    const facts = new WireSchemaEvidence(graph()).observe(frame({ id: "node", k: "chart", p: { min: 3 } }));
    expect(facts.has("property:TspChartProps.min")).toBe(true);
    expect(facts.has("property:TspCommonProps.min")).toBe(false);
  });

  test("absent optional props and untyped set payloads create no invented property facts", () => {
    const observer = new WireSchemaEvidence(graph());
    expect(observer.observe(frame({ id: "node", k: "button", p: {} })).has("property:TspButtonProps.disabled")).toBe(false);
    const row = frame(null);
    row.body = { sf: "surface", s: 2, ops: [["set", "node", { disabled: true }]] };
    const facts = observer.observe(row);
    expect(facts.has("operation:set")).toBe(true);
    expect(facts.has("property:TspButtonProps.disabled")).toBe(false);
  });

  test("a kind's array prop replaces the common object prop, which no longer constrains it", () => {
    const observer = new WireSchemaEvidence(graph());
    expect(observer.observe(frame({ id: "node", k: "button", p: { actions: ["confirm"] } })).has("property:TspButtonProps.actions")).toBe(true);
    expect(() => observer.observe(frame({ id: "node", k: "button", p: { actions: [7] } }))).toThrow("does not satisfy");
    expect(() => observer.observe(frame({ id: "node", k: "button", p: { actions: { click: "go" } } }))).toThrow("does not satisfy");
    expect(observer.observe(frame({ id: "node", k: "json", p: { value: 1, actions: { click: "go" } } })).has("property:TspCommonProps.actions.click")).toBe(true);
  });

  test("each Record key's value claims the owner's [key] member, and a wrong-typed value fails", () => {
    const schema = { ...graph(), TspPalette: alias(object([["colors", reference("Record", scalar("string"), scalar("string")), "property:TspPalette.colors"]])) };
    const row: WireObservation = { ...frame(null), verb: "t", body: { colors: { accent: "#fff" } } };
    const facts = new WireSchemaEvidence(schema).inspect(row);
    expect(facts.values.filter(item => item.entry === "property:TspPalette.colors[key]").map(item => [item.pointer, item.value])).toEqual([["/colors/accent", "#fff"]]);
    expect(() => new WireSchemaEvidence(schema).inspect({ ...row, body: { colors: { accent: 3 } } })).toThrow("does not satisfy");
  });
});
