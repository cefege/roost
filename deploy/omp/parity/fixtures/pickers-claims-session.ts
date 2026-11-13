//! Golden picker properties captured from the real OMP producers, keyed by scenario checkpoint.
//! Read back from the live DOM wire by the pickers-* proofs; regenerate when a producer's wire changes.
import type { Claim } from "./pickers-support";

export const pickerClaims: Record<string, Claim[]> = {
  "session-browse": [
    ["Column.id","when"],
    ["Column.format","time"],
    ["Column.format","dim"],
    ["Item.title","24 Oct 2026, 12:00"],
    ["Item.dot","success"],
    ["Item.dot","warning"],
    ["Item.badges[].title","Forked from “Alpha plan”"],
    ["Item.badges[].text","fork"],
    ["Item.dot","error"],
    ["Item.dot","pending"],
  ],
  "session-search": [
    ["Props.hits[key]",[[0,5]]],
    ["Props.hits[key][][0]",0],
    ["Props.hits[key][][1]",5],
  ],
  "session-delete": [
  ],
  "history-search": [
    ["Props.selected","1794000000-20m8fl9d9ryb"],
  ],
};
