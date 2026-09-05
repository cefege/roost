// KeeperContractV1 is the worker/keeper artifact identity used during survivor
// admission. Its implementation digest is generated from the transitive keeper
// bundle; protocol compatibility and exact artifact equality remain separate.

import { ROOST_BUILD_SHA } from "@roost/shared/build-identity";
import { supportedHostPlatform } from "@roost/shared/platform";
import { GENERATED_KEEPER_IMPLEMENTATION_DIGEST } from "./keeper-contract.generated.ts";
import {
  KEEPER_PROTOCOL_VERSION,
  KeeperContractV1Schema,
  REQUIRED_KEEPER_FEATURES,
  SUPPORTED_KEEPER_FEATURES,
  type KeeperContractV1,
} from "./protocol.ts";

export type { KeeperContractV1 } from "./protocol.ts";
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

export function keeperContractsProtocolCompatible(
  target: KeeperContractV1,
  running: KeeperContractV1,
): boolean {
  if (target.protocol_version !== running.protocol_version) return false;
  const targetSupported = new Set(target.supported_features);
  const runningSupported = new Set(running.supported_features);
  return target.required_features.every(feature => runningSupported.has(feature))
    && running.required_features.every(feature => targetSupported.has(feature));
}

export function keeperContractsSameImplementation(
  target: KeeperContractV1,
  running: KeeperContractV1,
): boolean {
  return target.implementation_digest !== null
    && running.implementation_digest !== null
    && target.implementation_digest === running.implementation_digest
    && target.bun_abi === running.bun_abi
    && target.platform === running.platform
    && target.arch === running.arch
    && sameStrings(target.supported_features, running.supported_features)
    && sameStrings(target.required_features, running.required_features)
    && target.protocol_version === running.protocol_version;
}

export function keeperContractsExactlyEqual(
  target: KeeperContractV1,
  running: KeeperContractV1,
): boolean {
  return keeperContractsSameImplementation(target, running)
    && target.build_sha === running.build_sha;
}

function sameStrings(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length
    && left.every((value, index) => value === right[index]);
}
