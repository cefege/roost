// Windows service-account normalization, shared by service-ctl, the binary
// installer, and the update broker. One copy exists because account
// spellings cross module boundaries during rollback proofs — a per-module
// normalization drift reads as a phantom config change.
//
// Account normalization deliberately strips BOTH local-machine prefixes
// (".\alice" and "./alice") before case-folding: SCM treats a bare name and
// its ".\"-qualified form as the same account, so equality proofs must too,
// and the operator-account denylist stays hole-free for either spelling.

/**
 * Canonical form used for every Windows service-account comparison: trim,
 * drop the machine prefix, case-fold en-US.
 */
export function normalizedWindowsAccount(account: string): string {
  return account.trim().replace(/^[.][\\/]/, "").toLocaleLowerCase("en-US");
}

