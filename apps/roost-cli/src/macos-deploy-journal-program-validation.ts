// Validation half of the remote macOS deploy journal program: strict schema
// parsing, canonical path confinement, keeper-update checks, and the durable
// read/write of the journal file itself. macos-deploy-journal-program.ts
// appends the action half and transmits the combined source to bun -e.

export const MACOS_DEPLOY_JOURNAL_PROGRAM_VALIDATION = String.raw`function reject(message) { throw new Error(message); } function canonicalAbsolute(value, name) {
  if (!value || !path.isAbsolute(value) || path.normalize(value) !== value || /[\r\n\0]/.test(value)) {
    reject(name + " is not a canonical absolute path");
  }
  return value;
}
function directReleasePath(value, sha) {
  canonicalAbsolute(value, "target release path");
  const relative = path.relative(releaseRoot, value);
  if (!relative || relative === ".." || relative.startsWith("../") || relative.includes("/")) {
    reject("target release path escapes the managed root");
  }
  if (sha !== null && (!shaPattern.test(sha) || !relative.startsWith(sha + "-")
    || !suffixPattern.test(relative.slice(sha.length + 1)))) {
    reject("target release identity is malformed");
  }
  if (fs.existsSync(releaseRoot)) {
    const rootStat = fs.lstatSync(releaseRoot);
    if (!rootStat.isDirectory() || rootStat.isSymbolicLink()
      || fs.realpathSync(releaseRoot) !== releaseRoot) reject("managed release root is not a canonical directory");
  }
  if (fs.existsSync(value)) {
    const targetStat = fs.lstatSync(value);
    if (!targetStat.isDirectory() || targetStat.isSymbolicLink()
      || fs.realpathSync(value) !== value) reject("managed release target is not a canonical directory");
  }
  return value;
}
function run(argv) {
  const result = Bun.spawnSync(argv, { stdin: "ignore", stdout: "pipe", stderr: "pipe" });
  return { exit: result.exitCode ?? 1, stdout: decoder.decode(result.stdout), stderr: decoder.decode(result.stderr) };
}
function syncDirectory(directory) {
  const fd = fs.openSync(directory, "r"); try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
}
function durableWrite(destination, bytes, mode) {
  const directory = path.dirname(destination);
  fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
  const temporary = destination + ".tmp-" + process.pid + "-" + randomUUID();
  let fd = null;
  try {
    fd = fs.openSync(temporary, "wx", mode);
    fs.writeFileSync(fd, bytes);
    fs.fsyncSync(fd);
    fs.closeSync(fd);
    fd = null;
    fs.chmodSync(temporary, mode);
    fs.renameSync(temporary, destination);
    syncDirectory(directory);
  } catch (error) {
    if (fd !== null) try { fs.closeSync(fd); } catch {}
    try { fs.rmSync(temporary, { force: true }); } catch {}
    throw error;
  }
}
function durableRemove(destination) {
  if (!fs.existsSync(destination)) return;
  fs.unlinkSync(destination);
  syncDirectory(path.dirname(destination));
}
function exactObject(value, fields, name) {
  if (!value || typeof value !== "object" || Array.isArray(value)) reject(name + " is not an object");
  const keys = Object.keys(value);
  if (keys.length !== fields.length || keys.some((key) => !fields.includes(key))) reject(name + " fields are malformed");
}
function boundedString(value, maximum) {
  return typeof value === "string" && value.length >= 1 && value.length <= maximum;
}
function normalizedOptionalIdentity(value, pattern, name, lowercase) {
  if (value === undefined || value === null || value === "") return null;
  if (typeof value !== "string" || !pattern.test(value)) reject(name + " is malformed");
  return lowercase ? value.toLowerCase() : value;
}
function featureList(value, name) {
  if (!Array.isArray(value) || value.length > 32
    || value.some((feature) => !boundedString(feature, 64))
    || value.some((feature, index) => index > 0 && value[index - 1] >= feature)) reject(name + " is malformed");
  return [...value];
}
function keeperContract(value, name) {
  exactObject(value, [
    "protocol_version", "supported_features", "required_features", "implementation_digest",
    "bun_abi", "platform", "arch", "build_sha",
  ], name);
  if (!Number.isSafeInteger(value.protocol_version) || value.protocol_version < 1
    || value.protocol_version > 0xffffffff) reject(name + " protocol is malformed");
  if (value.implementation_digest !== null && (!boundedString(value.implementation_digest, 64)
    || !digestPattern.test(value.implementation_digest))) reject(name + " implementation digest is malformed");
  if (!boundedString(value.bun_abi, 128) || !["darwin", "linux", "win32"].includes(value.platform)
    || !boundedString(value.arch, 64) || !boundedString(value.build_sha, 128)) reject(name + " runtime identity is malformed");
  return {
    protocol_version: value.protocol_version,
    supported_features: featureList(value.supported_features, name + " supported features"),
    required_features: featureList(value.required_features, name + " required features"),
    implementation_digest: value.implementation_digest, bun_abi: value.bun_abi,
    platform: value.platform, arch: value.arch, build_sha: value.build_sha,
  };
}
function sameKeeperImplementation(left, right) {
  return left.implementation_digest !== null && right.implementation_digest !== null
    && left.implementation_digest === right.implementation_digest && left.bun_abi === right.bun_abi
    && left.platform === right.platform && left.arch === right.arch
    && left.protocol_version === right.protocol_version
    && JSON.stringify(left.supported_features) === JSON.stringify(right.supported_features)
    && JSON.stringify(left.required_features) === JSON.stringify(right.required_features);
}
function keeperAdmission(value) {
  exactObject(value, [
    "classification", "source_contract_digest", "target_contract_digest", "expected_keeper_pid",
    "expected_keeper_epoch", "expected_binding_digest", "required_action",
  ], "keeper update admission");
  if (!["worker-only-safe", "keeper-restart-required"].includes(value.classification)
    || !boundedString(value.source_contract_digest, 64) || !digestPattern.test(value.source_contract_digest)
    || !boundedString(value.target_contract_digest, 64) || !digestPattern.test(value.target_contract_digest)
    || !Number.isSafeInteger(value.expected_keeper_pid) || value.expected_keeper_pid < 1
    || !boundedString(value.expected_keeper_epoch, 36) || !uuidPattern.test(value.expected_keeper_epoch)
    || !boundedString(value.expected_binding_digest, 64) || !digestPattern.test(value.expected_binding_digest)
    || !["preserve", "replace-empty"].includes(value.required_action)
    || (value.classification === "worker-only-safe") !== (value.required_action === "preserve")
    || (value.required_action === "replace-empty" && value.expected_binding_digest !== emptyBindingDigest)) {
    reject("keeper update admission is malformed");
  }
  return {
    classification: value.classification, source_contract_digest: value.source_contract_digest,
    target_contract_digest: value.target_contract_digest, expected_keeper_pid: value.expected_keeper_pid,
    expected_keeper_epoch: value.expected_keeper_epoch, expected_binding_digest: value.expected_binding_digest,
    required_action: value.required_action,
  };
}
function journaledKeeperUpdate(value) {
  if (value === null) return null;
  exactObject(value, ["admission", "source_contract", "target_contract"], "journaled keeper update");
  const admission = keeperAdmission(value.admission);
  const sourceContract = keeperContract(value.source_contract, "source keeper contract");
  const targetContract = keeperContract(value.target_contract, "target keeper contract");
  if (sourceContract.implementation_digest !== admission.source_contract_digest
    || targetContract.implementation_digest !== admission.target_contract_digest
    || (admission.classification === "worker-only-safe") !== sameKeeperImplementation(sourceContract, targetContract)) {
    reject("journaled keeper update contracts do not match admission");
  }
  return { admission, source_contract: sourceContract, target_contract: targetContract };
}
function decodedRequestedKeeperUpdate(workerFingerprint) {
  if ((requestedKeeperUpdateBase64 === "") !== (workerFingerprint === null)) reject("requested keeper update and worker fingerprint disagree");
  if (requestedKeeperUpdateBase64 === "") return null;
  if (requestedKeeperUpdateBase64.length > 32 * 1024) reject("requested keeper update is too large");
  const bytes = Buffer.from(requestedKeeperUpdateBase64, "base64");
  if (bytes.toString("base64") !== requestedKeeperUpdateBase64) reject("requested keeper update is not canonical base64");
  try {
    const keeperUpdate = journaledKeeperUpdate(JSON.parse(bytes.toString("utf8")));
    if (keeperUpdate === null) reject("requested keeper update is null");
    return keeperUpdate;
  } catch (error) {
    reject(error instanceof Error ? error.message : String(error));
  }
}
function requestedJournalFields() {
  const rolloutId = normalizedOptionalIdentity(requestedRollout, rolloutPattern, "requested rollout ID", true);
  const workerFingerprint = normalizedOptionalIdentity(
    requestedWorkerFingerprint, digestPattern, "requested worker fingerprint", false,
  );
  return {
    rolloutId, workerFingerprint,
    keeperUpdate: decodedRequestedKeeperUpdate(workerFingerprint),
  };
}
function validateJournal(value) {
  const journalFields = [
    "schemaVersion", "phase", "targetGitSha", "targetReleasePath", "rolloutId",
    "workerFingerprint", "keeperUpdate", "priorPlistBase64", "priorPlistMode",
    "priorLifecycle", "priorPid", "priorDisabled", "createdAt", "updatedAt",
  ];
  if (value === null || typeof value !== "object") reject("journal is not an object");
  if (value.schemaVersion !== 2 && value.schemaVersion !== 3) reject("journal schema is unsupported");
  // A v2 journal predates the durable-state observation; it upgrades with both
  // versions unknown, which can never authorize a roll-forward.
  const durableFields = value.schemaVersion === 2
    ? []
    : ["priorDurableStateVersion", "targetDurableStateVersion"];
  exactObject(value, [...journalFields, ...durableFields], "journal");
  const priorDurableStateVersion = value.schemaVersion === 2 ? null : value.priorDurableStateVersion;
  const targetDurableStateVersion = value.schemaVersion === 2 ? null : value.targetDurableStateVersion;
  for (const durableVersion of [priorDurableStateVersion, targetDurableStateVersion]) {
    if (durableVersion === null) continue;
    if (!Number.isSafeInteger(durableVersion) || durableVersion < 0 || durableVersion > 0xffffffff) {
      reject("journal durable state version is malformed");
    }
  }
  if (!["prepared", "activating", "activated", "committing", "rolling-back"].includes(value.phase)) reject("journal phase is malformed");
  if (typeof value.targetGitSha !== "string" || typeof value.targetReleasePath !== "string") reject("journal target identity is malformed");
  directReleasePath(value.targetReleasePath, value.targetGitSha);
  const rolloutId = normalizedOptionalIdentity(value.rolloutId, rolloutPattern, "journal rollout ID", true);
  const workerFingerprint = normalizedOptionalIdentity(value.workerFingerprint, digestPattern, "journal worker fingerprint", false);
  const keeperUpdate = journaledKeeperUpdate(value.keeperUpdate);
  if ((keeperUpdate === null) !== (workerFingerprint === null)) reject("journal keeper update and worker fingerprint disagree");
  if (!["unloaded", "loaded", "running"].includes(value.priorLifecycle)) reject("journal prior lifecycle is malformed");
  if (value.priorLifecycle === "running") {
    if (!Number.isSafeInteger(value.priorPid) || value.priorPid < 1) reject("journal prior process epoch is malformed");
  } else if (value.priorPid !== null) reject("journal has a process epoch for a non-running service");
  if (typeof value.priorDisabled !== "boolean") reject("journal disabled override is malformed");
  if (value.priorPlistBase64 === null) {
    if (value.priorPlistMode !== null || value.priorLifecycle !== "unloaded") reject("journal cannot restore a loaded service without plist bytes");
  } else {
    if (typeof value.priorPlistBase64 !== "string" || value.priorPlistBase64.length > 2 * 1024 * 1024
      || Buffer.from(value.priorPlistBase64, "base64").toString("base64") !== value.priorPlistBase64) {
      reject("journal prior plist bytes are malformed");
    }
    if (!Number.isSafeInteger(value.priorPlistMode) || value.priorPlistMode < 0
      || value.priorPlistMode > 0o777) reject("journal prior plist mode is malformed");
  }
  if (keeperUpdate !== null && value.priorPlistBase64 === null) reject("journal keeper update requires prior plist bytes to restore");
  if (typeof value.createdAt !== "string" || typeof value.updatedAt !== "string"
    || !Number.isFinite(Date.parse(value.createdAt)) || !Number.isFinite(Date.parse(value.updatedAt))) reject("journal timestamps are malformed");
  return {
    schemaVersion: 3, phase: value.phase,
    targetGitSha: value.targetGitSha, targetReleasePath: value.targetReleasePath,
    rolloutId, workerFingerprint, keeperUpdate,
    priorPlistBase64: value.priorPlistBase64, priorPlistMode: value.priorPlistMode,
    priorLifecycle: value.priorLifecycle, priorPid: value.priorPid,
    priorDisabled: value.priorDisabled,
    priorDurableStateVersion, targetDurableStateVersion,
    createdAt: value.createdAt, updatedAt: value.updatedAt,
  };
}
function readJournal() {
  if (!fs.existsSync(journalPath)) return null;
  const stat = fs.lstatSync(journalPath);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 3 * 1024 * 1024) reject("journal file is malformed");
  return validateJournal(JSON.parse(fs.readFileSync(journalPath, "utf8")));
}
function writeJournal(journal) {
  const checkedJournal = validateJournal(journal);
  durableWrite(journalPath, Buffer.from(JSON.stringify(checkedJournal) + "\n"), 0o600);
  return checkedJournal;
}
function emit(journal) {
  const payload = Buffer.from(JSON.stringify({ releaseRoot, journal })).toString("base64");
  console.log(outputPrefix + payload);
}
`;
