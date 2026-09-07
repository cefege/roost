// Validation and action body for the remote macOS deploy journal program.
// The environment preamble owns byte-stable imports, inputs, and constants.
// deploy-macos-journal-controller.ts transmits the combined source to bun -e.
import { MACOS_DEPLOY_JOURNAL_PROGRAM_ENVIRONMENT } from "./macos-deploy-journal-program-environment.ts";
export const MACOS_DEPLOY_JOURNAL_PROGRAM = MACOS_DEPLOY_JOURNAL_PROGRAM_ENVIRONMENT +
  String.raw`function reject(message) { throw new Error(message); } function canonicalAbsolute(value, name) {
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
  exactObject(value, [
    "schemaVersion", "phase", "targetGitSha", "targetReleasePath", "rolloutId",
    "workerFingerprint", "keeperUpdate", "priorPlistBase64", "priorPlistMode",
    "priorLifecycle", "priorPid", "priorDisabled", "createdAt", "updatedAt",
  ], "journal");
  if (value.schemaVersion !== 2) reject("journal schema is unsupported");
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
    schemaVersion: value.schemaVersion, phase: value.phase,
    targetGitSha: value.targetGitSha, targetReleasePath: value.targetReleasePath,
    rolloutId, workerFingerprint, keeperUpdate,
    priorPlistBase64: value.priorPlistBase64, priorPlistMode: value.priorPlistMode,
    priorLifecycle: value.priorLifecycle, priorPid: value.priorPid,
    priorDisabled: value.priorDisabled, createdAt: value.createdAt, updatedAt: value.updatedAt,
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
function launchdPrint() {
  return run(["launchctl", "print", "gui/" + String(process.getuid()) + "/" + label]);
}
function disabledOverride() {
  const result = run(["launchctl", "print-disabled", "gui/" + String(process.getuid())]);
  if (result.exit !== 0) reject("cannot read launchd disabled overrides: " + result.stderr);
  const overrideLine = result.stdout.split(/\r?\n/).find((line) => line.includes('"' + label + '"'));
  if (!overrideLine) return false;
  const match = overrideLine.match(/=>\s*(true|false|disabled|enabled)\s*$/);
  if (!match) reject("launchd disabled override is malformed: " + overrideLine.trim());
  return match[1] === "true" || match[1] === "disabled";
}
function exactPriorDefinition(journal) {
  if (journal.priorPlistBase64 === null) return !fs.existsSync(plistPath);
  if (!fs.existsSync(plistPath)) return false;
  const stat = fs.lstatSync(plistPath);
  return stat.isFile() && !stat.isSymbolicLink() && (stat.mode & 0o777) === journal.priorPlistMode
    && fs.readFileSync(plistPath).equals(Buffer.from(journal.priorPlistBase64, "base64"));
}
function removeRelease(releasePath, sha) {
  directReleasePath(releasePath, sha);
  if (!fs.existsSync(releasePath)) return;
  fs.rmSync(releasePath, { recursive: true, force: false });
  syncDirectory(releaseRoot);
}
function plistValue(bytes, key) {
  const temporary = journalPath + ".plist-probe-" + randomUUID();
  fs.writeFileSync(temporary, bytes, { mode: 0o600, flag: "wx" });
  try { const result = run(["/usr/libexec/PlistBuddy", "-c", "Print :" + key, temporary]);
    return result.exit === 0 ? result.stdout.trim() : null;
  } finally { fs.rmSync(temporary, { force: true }); }
}
try {
  canonicalAbsolute(journalPath, "journal path");
  canonicalAbsolute(releaseRoot, "release root");
  canonicalAbsolute(plistPath, "plist path");
  if (!label || /[\r\n\0/]/.test(label)) reject("launchd label is malformed");
  if (action === "load") {
    emit(readJournal());
  } else if (action === "prepare") {
    if (fs.existsSync(journalPath)) reject("an uncleared macOS deploy journal already exists");
    directReleasePath(requestedTarget, requestedSha);
    if (!fs.existsSync(requestedTarget)) reject("target release is not staged");
    const requestFields = requestedJournalFields();
    const launchd = launchdPrint();
    const lifecycle = launchd.exit !== 0 ? "unloaded"
      : /^\s*state = running\s*$/m.test(launchd.stdout) ? "running" : "loaded";
    const priorPidMatch = launchd.stdout.match(/^\s*pid = ([1-9]\d*)\s*$/m);
    const priorPid = lifecycle === "running" && priorPidMatch ? Number(priorPidMatch[1]) : null;
    if (lifecycle === "running" && !Number.isSafeInteger(priorPid)) reject("cannot capture the prior worker process epoch");
    let priorPlistBase64 = null;
    let priorPlistMode = null;
    if (fs.existsSync(plistPath)) {
      const stat = fs.lstatSync(plistPath);
      if (!stat.isFile() || stat.isSymbolicLink()) reject("worker plist is not a regular file");
      priorPlistBase64 = fs.readFileSync(plistPath).toString("base64");
      priorPlistMode = stat.mode & 0o777;
    } else if (lifecycle !== "unloaded") reject("cannot recover a loaded worker whose plist is absent");
    if (requestFields.keeperUpdate !== null && priorPlistBase64 === null) reject("keeper update requires prior plist bytes to restore");
    const priorDisabled = disabledOverride();
    if (lifecycle === "loaded" && !priorDisabled) reject("enabled KeepAlive worker has no durable loaded state");
    const priorBytes = priorPlistBase64 === null ? null : Buffer.from(priorPlistBase64, "base64");
    if (requestFields.keeperUpdate && (!priorBytes || !shaPattern.test(plistValue(priorBytes, "EnvironmentVariables:GIT_SHA") || ""))) reject("prior worker build identity is unavailable");
    const now = new Date().toISOString();
    const journal = {
      schemaVersion: 2, phase: "prepared",
      targetGitSha: requestedSha, targetReleasePath: requestedTarget,
      rolloutId: requestFields.rolloutId, workerFingerprint: requestFields.workerFingerprint,
      keeperUpdate: requestFields.keeperUpdate,
      priorPlistBase64, priorPlistMode, priorLifecycle: lifecycle, priorPid,
      priorDisabled, createdAt: now, updatedAt: now,
    };
    const persistedJournal = writeJournal(journal);
    emit(persistedJournal);
  } else if (action === "checkpoint-activating") {
    const journal = readJournal();
    if (!journal || journal.phase !== "prepared") reject("prepared journal is missing");
    directReleasePath(requestedTarget, requestedSha);
    const requestFields = requestedJournalFields();
    if (journal.targetGitSha !== requestedSha
      || journal.targetReleasePath !== requestedTarget
      || journal.rolloutId !== requestFields.rolloutId
      || journal.workerFingerprint !== requestFields.workerFingerprint
      || JSON.stringify(journal.keeperUpdate) !== JSON.stringify(requestFields.keeperUpdate)) {
      reject("activation checkpoint target does not match the prepared journal");
    }
    const activating = writeJournal({ ...journal, phase: "activating", updatedAt: new Date().toISOString() });
    emit(activating);
  } else if (action === "checkpoint-activated") {
    const journal = readJournal();
    if (!journal || journal.phase !== "activating") reject("activating journal is missing");
    directReleasePath(requestedTarget, requestedSha);
    const requestFields = requestedJournalFields();
    if (journal.targetGitSha !== requestedSha
      || journal.targetReleasePath !== requestedTarget
      || journal.rolloutId !== requestFields.rolloutId
      || journal.workerFingerprint !== requestFields.workerFingerprint
      || JSON.stringify(journal.keeperUpdate) !== JSON.stringify(requestFields.keeperUpdate)) {
      reject("activated checkpoint target does not match the activating journal");
    }
    const activated = writeJournal({ ...journal, phase: "activated", updatedAt: new Date().toISOString() });
    emit(activated);
  } else if (action === "checkpoint-rollback") {
    const journal = readJournal();
    if (!journal || journal.phase === "committing") reject("rollback checkpoint is unavailable");
    const rolling = journal.phase === "rolling-back" ? journal
      : writeJournal({ ...journal, phase: "rolling-back", updatedAt: new Date().toISOString() });
    emit(rolling);
  } else if (action === "checkpoint-commit") {
    const journal = readJournal();
    if (!journal || !["activating", "activated", "committing"].includes(journal.phase)) reject("commit checkpoint is unavailable");
    const committing = journal.phase === "committing" ? journal
      : writeJournal({ ...journal, phase: "committing", updatedAt: new Date().toISOString() });
    emit(committing);
  } else if (action === "restore-prior") {
    const journal = readJournal();
    if (!journal || !["prepared", "activating", "activated", "rolling-back"].includes(journal.phase)) reject("rollback journal is missing");
    if (journal.priorPlistBase64 === null) durableRemove(plistPath);
    else durableWrite(
      plistPath,
      Buffer.from(journal.priorPlistBase64, "base64"),
      journal.priorPlistMode,
    );
    if (!exactPriorDefinition(journal)) reject("prior plist did not round-trip exactly");
  } else if (action === "prove-prior-definition") {
    const journal = readJournal();
    if (!journal || !exactPriorDefinition(journal)) reject("prior plist definition is not restored");
    console.log("RoostPriorDefinitionMatch=yes");
  } else if (action === "remove-target") {
    const journal = readJournal();
    if (!journal) reject("journal is missing");
    removeRelease(journal.targetReleasePath, journal.targetGitSha);
  } else if (action === "cleanup-prior") {
    const journal = readJournal();
    if (!journal || !["activating", "activated", "committing"].includes(journal.phase)) reject("activated journal is missing");
    if (journal.priorPlistBase64 !== null) {
      const priorPath = plistValue(Buffer.from(journal.priorPlistBase64, "base64"), "WorkingDirectory");
      if (priorPath && priorPath !== journal.targetReleasePath) {
        const protectedPaths = new Set();
        for (const name of fs.readdirSync(path.dirname(plistPath))) {
          if (!name.endsWith(".plist")) continue;
          const candidate = path.join(path.dirname(plistPath), name);
          const candidateStat = fs.lstatSync(candidate);
          if (!candidateStat.isFile() || candidateStat.isSymbolicLink()) continue;
          const result = run(["/usr/libexec/PlistBuddy", "-c", "Print :WorkingDirectory", candidate]);
          if (result.exit === 0 && result.stdout.trim()
            && path.isAbsolute(result.stdout.trim())) {
            protectedPaths.add(path.normalize(result.stdout.trim()));
          }
        }
        if (!protectedPaths.has(priorPath)) {
          let managed = true;
          try {
            directReleasePath(priorPath, null);
          } catch {
            managed = false;
          }
          if (managed) removeRelease(priorPath, null);
        }
      }
    }
  } else if (action === "clear") {
    if (!readJournal()) reject("journal is missing");
    durableRemove(journalPath);
  } else {
    reject("unknown macOS deploy journal action");
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 65;
}
`;
