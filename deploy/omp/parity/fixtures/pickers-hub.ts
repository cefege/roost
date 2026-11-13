//! Model hub proof scenarios: the catalogue, role-assignment strip and roles views of the real hub overlay,
//! each proving the picker properties the live wire carries. Registered by pickers.ts.
import type { Checkpoint, FixtureCase } from "../scenario";
import { pickerClaims } from "./pickers-claims-hub";
import { createModelHub } from "./pickers-hub-support";
import { below, claimOf, coversFor, literal, without, type Claim } from "./pickers-support";

const heading = { terminal: "Models", browser: 'h2:text-is("Models")' };
const alphaRow = { terminal: "parity/alpha", browser: '[data-omp-picker-item="parity/alpha"]' };
const roleRow = { terminal: "parity/alpha", browser: '[data-omp-picker-item="role:default"]' };

const catalogue = pickerClaims["hub-all"]!;
const strip = pickerClaims["hub-strip"]!;
const roles = pickerClaims["hub-roles"]!;
const literalEntries = ["Props.title", "Props.subtitle", "Item.label", "Item.id"];

const hubCase = (id: string, claims: Claim[][], checkpoints: Checkpoint[]): FixtureCase => ({
  id, covers: coversFor(...claims), create: createModelHub, checkpoints,
});

const reassignment: Omit<Checkpoint, "comparisons"> = {
  name: "role reassigned",
  terminalInput: "\r\x1b[B\r",
  browserActions: [{ type: "click", selector: '[data-omp-picker-item="parity/alpha"]' }, { type: "press", key: "Enter" }, { type: "press", key: "ArrowDown" }, { type: "press", key: "Enter" }],
};

export const fixtureCases: FixtureCase[] = [
  hubCase("picker-hub-catalogue", [catalogue], [{
    name: "catalogue",
    comparisons: [
      literal("title", "(Models)", { selector: 'h2:text-is("Models")' }, "Models", [claimOf(catalogue, "Props.title")]),
      literal("subtitle", "All available models · (--models scope)", { selector: 'p:text-is("--models scope")' }, "--models scope", [claimOf(catalogue, "Props.subtitle")]),
      literal("item label", "(parity/alpha)", { selector: '[data-omp-picker-item="parity/alpha"] :text-is("parity/alpha")' }, "parity/alpha", [claimOf(catalogue, "Item.label")]),
      literal("item id", "(parity/alpha)", { selector: '[data-omp-picker-item="parity/alpha"]', attribute: "data-omp-picker-item" }, "parity/alpha", [claimOf(catalogue, "Item.id")]),
      below("catalogue properties", alphaRow, heading, without(catalogue, ...literalEntries)),
    ],
  }]),
  hubCase("picker-hub-strip", [strip], [{
    name: "assignment strip",
    terminalInput: "\r",
    browserActions: [{ type: "click", selector: '[data-omp-picker-item="parity/alpha"]' }, { type: "press", key: "Enter" }],
    comparisons: [below("strip properties", alphaRow, heading, strip)],
  }]),
  hubCase("picker-hub-roles", [roles], [{
    name: "roles view",
    terminalInput: "\x1b[A",
    browserActions: [{ type: "click", selector: 'button:has-text("Roles")' }],
    comparisons: [below("roles properties", roleRow, heading, roles)],
  }]),
  {
    id: "picker-hub-patch",
    covers: ["kind:picker"],
    create: context => createModelHub(context, { extraModels: 9 }),
    checkpoints: [{ ...reassignment, comparisons: [below("patched rows", alphaRow, heading, [])] }, { name: "after assignment", comparisons: [below("patched rows", alphaRow, heading, [])] }],
  },
  {
    id: "picker-hub-refresh",
    covers: ["kind:picker"],
    create: context => createModelHub(context, { extraModels: 9, scoped: false, dropOnAssign: "parity/extra-09" }),
    checkpoints: [{ ...reassignment, comparisons: [below("refreshed rows", alphaRow, heading, [])] }, { name: "after assignment", comparisons: [below("refreshed rows", alphaRow, heading, [])] }],
  },
  {
    id: "picker-hub-providers",
    covers: ["kind:picker"],
    create: context => createModelHub(context, { scoped: false, lockedProviders: ["sealed"], discovery: { parity: "ok", other: "unavailable" } }),
    checkpoints: [{ name: "provider scopes", comparisons: [below("provider scopes", alphaRow, heading, [])] }],
  },
  {
    id: "picker-hub-discovery",
    covers: ["kind:picker"],
    create: context => createModelHub(context, { scoped: false, discovery: { parity: "cached", other: "unauthenticated" } }),
    checkpoints: [{ name: "provider scopes", comparisons: [below("provider scopes", alphaRow, heading, [])] }],
  },
];
