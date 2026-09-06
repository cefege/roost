// KeeperContractV1 is the worker/keeper artifact identity used during survivor
// admission. Its implementation digest is generated from the transitive keeper
// bundle; protocol compatibility and exact artifact equality remain separate.

import { ROOST_BUILD_SHA } from "@roost/shared/build-identity";
import {
  KeeperContractV1Schema,
  keeperContractsExactlyEqual,
  keeperContractsProtocolCompatible,
  keeperContractsSameImplementation,
  type KeeperContractV1,
} from "@roost/shared/keeper-update";
import { supportedHostPlatform } from "@roost/shared/platform";
import { GENERATED_KEEPER_IMPLEMENTATION_DIGEST } from "./keeper-contract.generated.ts";
import {
  KEEPER_PROTOCOL_VERSION,
  REQUIRED_KEEPER_FEATURES,
  SUPPORTED_KEEPER_FEATURES,
} from "./protocol.ts";

export {
  keeperContractsExactlyEqual,
  keeperContractsProtocolCompatible,
  keeperContractsSameImplementation,
};
export type { KeeperContractV1 };
declare const __ROOST_KEEPER_IMPLEMENTATION_DIGEST__: string | undefined;

const SHA256_DIGEST = /^[0-9a-f]{64}$/;

function implementationDigest(): string | null {
  const compiledDigest =
    typeof __ROOST_KEEPER_IMPLEMENTATION_DIGEST__ === "string"
      ? __ROOST_KEEPER_IMPLEMENTATION_DIGEST__
      : null;
  const candidate = compiledDigest ?? GENERATED_KEEPER_IMPLEMENTATION_DIGEST;
  return candidate !== null && SHA256_DIGEST.test(candidate) ? candidate : null;
}

export const KEEPER_TARGET_CONTRACT: KeeperContractV1 =
  KeeperContractV1Schema.parse({
    protocol_version: KEEPER_PROTOCOL_VERSION,
    supported_features: [...SUPPORTED_KEEPER_FEATURES].sort(),
    required_features: [...REQUIRED_KEEPER_FEATURES].sort(),
    implementation_digest: implementationDigest(),
    bun_abi: Bun.version,
    platform: supportedHostPlatform(),
    arch: process.arch,
    build_sha: ROOST_BUILD_SHA,
  });

