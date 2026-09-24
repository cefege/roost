// Converts static, display-safe host identity between shared wire and protobuf.
// Worker registration and heartbeat use the empty protobuf message to attest
// collection on modern workers; persisted Worker records omit null identity.

import { create } from "@bufbuild/protobuf";
import {
  HostIdentitySchema,
  type HostIdentity as HostIdentityProto,
} from "./gen/roost/v1/wire_pb.ts";
import { normalizeHostIdentity, type HostIdentity } from "./wire/worker.ts";

export function hostIdentityToProto(
  identity: HostIdentity | null,
): HostIdentityProto {
  const normalized = identity === null ? null : normalizeHostIdentity(identity);
  return create(HostIdentitySchema, {
    hardwareModel: normalized?.hardware_model ?? undefined,
    chip: normalized?.chip ?? undefined,
    linuxDistribution: normalized?.linux_distribution ?? undefined,
  });
}

export function hostIdentityFromProto(
  identity: HostIdentityProto | undefined,
): HostIdentity | null {
  if (identity === undefined) return null;
  return normalizeHostIdentity({
    hardware_model: identity.hardwareModel,
    chip: identity.chip,
    linux_distribution: identity.linuxDistribution,
  });
}
