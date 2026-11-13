//! Golden picker properties captured from the real OMP producers, keyed by scenario checkpoint.
//! Read back from the live DOM wire by the pickers-* proofs; regenerate when a producer's wire changes.
import type { Claim } from "./pickers-support";

export const pickerClaims: Record<string, Claim[]> = {
  "tree-turns": [
    ["Props.layout","tree"],
    ["Props.selected","u2"],
    ["Item.depth",0],
    ["Item.node","user"],
    ["Item.dot","accent"],
    ["Item.node","marker"],
    ["Item.node","assistant"],
    ["Item.open",true],
    ["Item.role","omp.tool.read"],
    ["Item.node","tool"],
    ["Item.badges[].tone","warning"],
  ],
  "tree-label": [
    ["Props.focus","preview"],
  ],
};
