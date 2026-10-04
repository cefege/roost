// Keeper update admission is the shared fail-closed contract for worker,
// coordinator, and CLI rollout code. It validates runtime proof and classifies
// whether a worker-only restart can preserve the keeper or must replace it empty.

import { z } from "zod";

const SHA256_DIGEST = /^[0-9a-f]{64}$/;
const SortedContractFeatureList = z.array(z.string().min(1).max(64))
  .max(32)
  .refine(
    features => features.every((feature, index) =>
      index === 0 || features[index - 1]! < feature),
    "keeper contract features must be sorted and unique",
  )
  .readonly();

export const KeeperContractV1Schema = z.object({
  protocol_version: z.number().int().min(1).max(0xffff_ffff),
  supported_features: SortedContractFeatureList,
  required_features: SortedContractFeatureList,
  implementation_digest: z.string().regex(SHA256_DIGEST).nullable(),
  bun_abi: z.string().min(1).max(128),
  platform: z.enum(["darwin", "linux", "win32"]),
  arch: z.string().min(1).max(64),
  build_sha: z.string().min(1).max(128),
}).strict().readonly();
export type KeeperContractV1 = z.infer<typeof KeeperContractV1Schema>;

export const KEEPER_EMPTY_BINDING_DIGEST =
  "74eb8cfffb89f155db2201d8c1b13202c29d91be6cc3d4fec6b465c9a9ede627";
export const KEEPER_BINDING_DIGEST_PREAMBLE = "keeper-bindings-v1\n";

export const KeeperRuntimeObservationV1Schema = z.object({
  schema_version: z.literal(1),
  running_contract: KeeperContractV1Schema,
  keeper_pid: z.number().int().positive().max(Number.MAX_SAFE_INTEGER),
  keeper_epoch: z.string().uuid(),
  channel_count: z.number().int().nonnegative().max(0xffff),
  binding_digest: z.string().regex(SHA256_DIGEST),
  reconciled_at_ms: z.number().int().positive().max(Number.MAX_SAFE_INTEGER),
}).strict().readonly().superRefine((observation, context) => {
  if ((observation.channel_count === 0)
    !== (observation.binding_digest === KEEPER_EMPTY_BINDING_DIGEST)) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      message: "keeper channel count and binding digest disagree",
      path: ["binding_digest"],
    });
  }
});
export type KeeperRuntimeObservationV1 = z.infer<
  typeof KeeperRuntimeObservationV1Schema
>;

export const KeeperUpdateClassificationSchema = z.enum([
  "worker-only-safe",
  "keeper-restart-required",
  "incompatible-with-live-sessions",
  "unproven",
]);
export type KeeperUpdateClassification = z.infer<
  typeof KeeperUpdateClassificationSchema
>;

export const KeeperUpdateRequiredActionSchema = z.enum([
  "preserve",
  "replace-empty",
]);
export type KeeperUpdateRequiredAction = z.infer<
  typeof KeeperUpdateRequiredActionSchema
>;

export const KeeperUpdateOutcomeSchema = z.enum([
  "preserved",
  "already-converged",
  "shutdown",
  "already-absent",
]);
export type KeeperUpdateOutcome = z.infer<typeof KeeperUpdateOutcomeSchema>;

export function keeperUpdateOutcomeMatchesAction(
  action: KeeperUpdateRequiredAction | "maintenance",
  outcome: string,
): outcome is KeeperUpdateOutcome {
  if (action === "preserve") return outcome === "preserved";
  if (action === "maintenance") {
    return outcome === "shutdown" || outcome === "already-absent";
  }
  return outcome === "shutdown"
    || outcome === "already-absent"
    || outcome === "already-converged";
}

export const KeeperCoordinatorOpenSessionIdsSchema = z.array(
  z.string().uuid(),
).max(65_535).refine(
  sessionIds => sessionIds.every((sessionId, index) =>
    index === 0 || sessionIds[index - 1]! < sessionId),
  "coordinator session IDs must be sorted and unique",
).readonly();

export const KeeperUpdateAdmissionV1Schema = z.object({
  classification: KeeperUpdateClassificationSchema.exclude([
    "incompatible-with-live-sessions",
    "unproven",
  ]),
  source_contract_digest: z.string().regex(SHA256_DIGEST),
  target_contract_digest: z.string().regex(SHA256_DIGEST),
  expected_keeper_pid: z.number().int().positive().max(Number.MAX_SAFE_INTEGER),
  expected_keeper_epoch: z.string().uuid(),
  expected_binding_digest: z.string().regex(SHA256_DIGEST),
  required_action: KeeperUpdateRequiredActionSchema,
}).strict().readonly().superRefine((admission, context) => {
  const expectedAction = admission.classification === "worker-only-safe"
    ? "preserve"
    : "replace-empty";
  if (admission.required_action !== expectedAction) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      message: "keeper update classification and required action disagree",
      path: ["required_action"],
    });
  }
  if (admission.required_action === "replace-empty"
    && admission.expected_binding_digest !== KEEPER_EMPTY_BINDING_DIGEST) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      message: "replace-empty requires the canonical empty binding digest",
      path: ["expected_binding_digest"],
    });
  }
  if (admission.required_action === "preserve"
    && admission.source_contract_digest !== admission.target_contract_digest) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      message: "preserve requires equal source and target implementation digests",
      path: ["target_contract_digest"],
    });
  }
});
export type KeeperUpdateAdmissionV1 = z.infer<
  typeof KeeperUpdateAdmissionV1Schema
>;
export const JournaledKeeperUpdateV1Schema = z.object({
  admission: KeeperUpdateAdmissionV1Schema,
  source_contract: KeeperContractV1Schema,
  target_contract: KeeperContractV1Schema,
}).strict().readonly().superRefine((update, context) => {
  if (update.source_contract.implementation_digest
      !== update.admission.source_contract_digest
    || update.target_contract.implementation_digest
      !== update.admission.target_contract_digest) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      message: "journaled keeper contract digests disagree with admission",
      path: ["admission"],
    });
  }
  const sameImplementation = keeperContractsSameImplementation(
    update.target_contract,
    update.source_contract,
  );
  if ((update.admission.classification === "worker-only-safe")
    !== sameImplementation) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      message: "journaled keeper contracts disagree with classification",
      path: ["admission", "classification"],
    });
  }
});
export type JournaledKeeperUpdateV1 = z.infer<
  typeof JournaledKeeperUpdateV1Schema
>;

/** Canonical bytes hashed by the worker for active and in-flight bindings. */
export function keeperBindingDigestInput(
  bindings: readonly { channel_id: number; pid: number }[],
  spawningChannels: readonly number[] = [],
): string {
  const active = [...bindings].sort((left, right) => left.channel_id - right.channel_id);
  const spawning = [...spawningChannels].sort((left, right) => left - right);
  return KEEPER_BINDING_DIGEST_PREAMBLE
    + active.map(binding => `b:${binding.channel_id}:${binding.pid}\n`).join("")
    + spawning.map(channelId => `s:${channelId}\n`).join("");
}

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

/** build_sha is provenance only and deliberately excluded from restart admission. */
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

export function classifyKeeperUpdate(
  targetContract: KeeperContractV1,
  observation: KeeperRuntimeObservationV1 | null | undefined,
  coordinatorOpenSessionIds: ReadonlySet<string>,
): KeeperUpdateClassification {
  const target = KeeperContractV1Schema.safeParse(targetContract);
  const observed = KeeperRuntimeObservationV1Schema.safeParse(observation);
  if (!target.success || !observed.success) return "unproven";
  const runtime = observed.data;
  if (target.data.implementation_digest === null
    || runtime.running_contract.implementation_digest === null
    || coordinatorOpenSessionIds.size !== runtime.channel_count) {
    return "unproven";
  }
  if (keeperContractsSameImplementation(target.data, runtime.running_contract)) {
    return "worker-only-safe";
  }
  return runtime.channel_count === 0 && coordinatorOpenSessionIds.size === 0
    ? "keeper-restart-required"
    : "incompatible-with-live-sessions";
}

export function keeperUpdateAdmission(
  targetContract: KeeperContractV1,
  observation: KeeperRuntimeObservationV1 | null | undefined,
  coordinatorOpenSessionIds: ReadonlySet<string>,
): KeeperUpdateAdmissionV1 | null {
  const classification = classifyKeeperUpdate(
    targetContract,
    observation,
    coordinatorOpenSessionIds,
  );
  if (!observation
    || (classification !== "worker-only-safe"
      && classification !== "keeper-restart-required")) {
    return null;
  }
  const sourceContractDigest = observation.running_contract.implementation_digest;
  const targetContractDigest = targetContract.implementation_digest;
  if (!sourceContractDigest || !targetContractDigest) return null;
  return KeeperUpdateAdmissionV1Schema.parse({
    classification,
    source_contract_digest: sourceContractDigest,
    target_contract_digest: targetContractDigest,
    expected_keeper_pid: observation.keeper_pid,
    expected_keeper_epoch: observation.keeper_epoch,
    expected_binding_digest: observation.binding_digest,
    required_action: classification === "worker-only-safe"
      ? "preserve"
      : "replace-empty",
  });
}

function sameStrings(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length
    && left.every((value, index) => value === right[index]);
}
