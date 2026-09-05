// Starts the real keeper implementation with an overridden contract.
// keeper-survivor-continuity.test.ts uses it to exercise compatible adoption
// and protocol-incompatible replacement against the production PTY host.

import { runKeeper } from "../src/keeper/multiplexed-main.ts";
import { KeeperContractV1Schema } from "../src/keeper/protocol.ts";
import { KEEPER_TARGET_CONTRACT } from "../src/keeper/keeper-stamp.ts";

const endpoint = process.argv[2];
const mode = process.argv[3];
if (!endpoint || (mode !== "compatible" && mode !== "incompatible")) {
  throw new Error("keeper fixture endpoint and mode are required");
}

const contract = KeeperContractV1Schema.parse(mode === "compatible"
  ? {
      ...KEEPER_TARGET_CONTRACT,
      implementation_digest: "f".repeat(64),
      build_sha: "compatible-prior-build",
    }
  : {
      ...KEEPER_TARGET_CONTRACT,
      required_features: [
        ...KEEPER_TARGET_CONTRACT.required_features,
        "fixture_worker_requirement_v1",
      ].sort(),
      build_sha: "incompatible-fixture",
    });

runKeeper(endpoint, contract);
