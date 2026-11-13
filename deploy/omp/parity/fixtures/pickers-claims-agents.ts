//! Golden picker properties captured from the real OMP producers, keyed by scenario checkpoint.
//! Read back from the live DOM wire by the pickers-* proofs; regenerate when a producer's wire changes.
import type { Claim } from "./pickers-support";

export const pickerClaims: Record<string, Claim[]> = {
  "agent-roster": [
    ["Props.selected","FixtureScout"],
    ["Action.on",false],
    ["Item.dot","muted"],
    ["Item.chips[].text","fixture-coder"],
    ["Item.chips[].dot","thinkingHigh"],
  ],
  "agent-tree": [
    ["Action.on",true],
  ],
  "agents-library": [
    ["Props.selected","agent:project:FixtureScout"],
    ["Scope.disabled","No bundled agents"],
    ["Column.min",8],
    ["Item.disabled","Disabled · space to enable"],
  ],
  "hook-options": [
    ["Props.selected","0"],
  ],
};
