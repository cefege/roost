// Byte-stable environment preamble for the remote macOS journal program.
// macos-deploy-journal-program.ts prepends it to validation and action code.
// Literal dynamic imports are required because bun -e has no module location.

export const MACOS_DEPLOY_JOURNAL_PROGRAM_ENVIRONMENT = String.raw`
// This program is transmitted to bun -e and has no module file from which
// static imports could resolve; literal dynamic imports are the remote boundary.
const fs = await import("node:fs");
const path = await import("node:path");
const { randomUUID } = await import("node:crypto");
const decoder = new TextDecoder();
const action = process.env.ROOST_MAC_DEPLOY_ACTION ?? "";
const journalPath = process.env.ROOST_MAC_DEPLOY_JOURNAL ?? "";
const releaseRoot = process.env.ROOST_MAC_DEPLOY_RELEASE_ROOT ?? "";
const plistPath = process.env.ROOST_MAC_DEPLOY_PLIST ?? "";
const label = process.env.ROOST_MAC_DEPLOY_LABEL ?? "";
const requestedSha = process.env.ROOST_MAC_DEPLOY_TARGET_SHA ?? "";
const requestedTarget = process.env.ROOST_MAC_DEPLOY_TARGET_PATH ?? "";
const requestedRollout = process.env.ROOST_MAC_DEPLOY_ROLLOUT_ID || null;
const requestedWorkerFingerprint = process.env.ROOST_MAC_DEPLOY_WORKER_FINGERPRINT || null;
const requestedKeeperUpdateBase64 = process.env.ROOST_MAC_DEPLOY_KEEPER_UPDATE ?? "";
const outputPrefix = "RoostMacDeployJournal=";
const shaPattern = /^[a-f0-9]{40,64}$/;
const suffixPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const rolloutPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const digestPattern = /^[0-9a-f]{64}$/;
const emptyBindingDigest = "74eb8cfffb89f155db2201d8c1b13202c29d91be6cc3d4fec6b465c9a9ede627";
const uuidPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
`;
