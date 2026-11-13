//! Golden picker properties captured from the real OMP producers, keyed by scenario checkpoint.
//! Read back from the live DOM wire by the pickers-* proofs; regenerate when a producer's wire changes.
import type { Claim } from "./pickers-support";

export const pickerClaims: Record<string, Claim[]> = {
  "model-rich": [
    ["Props.preview","below"],
    ["Props.selected","parity/beta"],
  ],
  "copy-timeline": [
    ["Props.layout","timeline"],
    ["Props.selected","u3"],
  ],
};
