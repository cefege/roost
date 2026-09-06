// Hidden release-artifact probe for POSIX self-update admission.
// The updater executes the downloaded candidate before replacing itself so
// keeper ABI/platform metadata comes from the exact target runtime.

import { KEEPER_TARGET_CONTRACT } from "../../worker/src/keeper/keeper-stamp.ts";

export function keeperContractCommand(args: readonly string[]): void {
  if (args.length !== 0) throw new Error("internal keeper contract probe accepts no arguments");
  console.log(JSON.stringify(KEEPER_TARGET_CONTRACT));
}
