//! Golden picker properties captured from the real OMP producers, keyed by scenario checkpoint.
//! Read back from the live DOM wire by the pickers-* proofs; regenerate when a producer's wire changes.
import type { Claim } from "./pickers-support";

export const pickerClaims: Record<string, Claim[]> = {
  "reset-accounts": [
    ["Props.layout","cards"],
    ["Props.size","md"],
    ["Props.preview","none"],
    ["Props.selected","parity:1:c1"],
    ["Item.detail",[{"t":"Parity · #1"}]],
    ["Item.disabled","cooldown active"],
    ["Column.priority",1],
    ["Column.id","expires"],
  ],
  "reset-confirm": [
    ["Props.confirm.act","confirm"],
    ["Props.confirm.text","Spend 1 saved reset on work@example.com?"],
  ],
  "logout-accounts": [
    ["Props.selected","1"],
    ["Action.danger",true],
  ],
};
