// Pure adapters between generated protobuf keeper proof messages and the
// shared update-admission Zod contracts. Worker heartbeat and coordinator row
// projection use these so bigint and optional-field handling cannot diverge.

import { create } from "@bufbuild/protobuf";
import {
  KeeperContractV1Schema as KeeperContractProtoSchema,
  KeeperRuntimeObservationV1Schema as KeeperRuntimeProtoSchema,
  type KeeperContractV1 as KeeperContractProto,
  type KeeperRuntimeObservationV1 as KeeperRuntimeProto,
} from "./gen/roost/v1/wire_pb.ts";
import {
  KeeperContractV1Schema,
  KeeperRuntimeObservationV1Schema,
  type KeeperContractV1,
  type KeeperRuntimeObservationV1,
} from "./keeper-update.ts";

export function keeperContractToProto(
  contract: KeeperContractV1,
): KeeperContractProto {
  const checked = KeeperContractV1Schema.parse(contract);
  return create(KeeperContractProtoSchema, {
    protocolVersion: checked.protocol_version,
    supportedFeatures: [...checked.supported_features],
    requiredFeatures: [...checked.required_features],
    implementationDigest: checked.implementation_digest ?? undefined,
    bunAbi: checked.bun_abi,
    platform: checked.platform,
    arch: checked.arch,
    buildSha: checked.build_sha,
  });
}

export function keeperContractFromProto(
  contract: KeeperContractProto,
): KeeperContractV1 {
  return KeeperContractV1Schema.parse({
    protocol_version: contract.protocolVersion,
    supported_features: contract.supportedFeatures,
    required_features: contract.requiredFeatures,
    implementation_digest: contract.implementationDigest ?? null,
    bun_abi: contract.bunAbi,
    platform: contract.platform,
    arch: contract.arch,
    build_sha: contract.buildSha,
  });
}

export function keeperRuntimeObservationToProto(
  observation: KeeperRuntimeObservationV1,
): KeeperRuntimeProto {
  const checked = KeeperRuntimeObservationV1Schema.parse(observation);
  return create(KeeperRuntimeProtoSchema, {
    schemaVersion: checked.schema_version,
    runningContract: keeperContractToProto(checked.running_contract),
    keeperPid: BigInt(checked.keeper_pid),
    keeperEpoch: checked.keeper_epoch,
    channelCount: checked.channel_count,
    bindingDigest: checked.binding_digest,
    reconciledAtMs: BigInt(checked.reconciled_at_ms),
  });
}

export function keeperRuntimeObservationFromProto(
  observation: KeeperRuntimeProto,
): KeeperRuntimeObservationV1 {
  if (!observation.runningContract) {
    throw new Error("keeper runtime observation is missing its running contract");
  }
  return KeeperRuntimeObservationV1Schema.parse({
    schema_version: observation.schemaVersion,
    running_contract: keeperContractFromProto(observation.runningContract),
    keeper_pid: Number(observation.keeperPid),
    keeper_epoch: observation.keeperEpoch,
    channel_count: observation.channelCount,
    binding_digest: observation.bindingDigest,
    reconciled_at_ms: Number(observation.reconciledAtMs),
  });
}
