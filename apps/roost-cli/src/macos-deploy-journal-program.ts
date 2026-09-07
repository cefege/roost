// Action half of the remote macOS deploy journal program and the assembly of
// the transmitted source: environment preamble, validation, durable-state
// probe, then the launchd/plist actions dispatched by ROOST_MAC_DEPLOY_ACTION.
// deploy-macos-journal-controller.ts transmits the result to bun -e.
import { DURABLE_WORKER_STATE_PROBE_JS } from "./durable-worker-state.ts";
import { MACOS_DEPLOY_JOURNAL_PROGRAM_ENVIRONMENT } from "./macos-deploy-journal-program-environment.ts";
import { MACOS_DEPLOY_JOURNAL_PROGRAM_VALIDATION } from "./macos-deploy-journal-program-validation.ts";
export const MACOS_DEPLOY_JOURNAL_PROGRAM = MACOS_DEPLOY_JOURNAL_PROGRAM_ENVIRONMENT +
  MACOS_DEPLOY_JOURNAL_PROGRAM_VALIDATION + DURABLE_WORKER_STATE_PROBE_JS +
  String.raw`function launchdPrint() {
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
      schemaVersion: 3, phase: "prepared",
      targetGitSha: requestedSha, targetReleasePath: requestedTarget,
      rolloutId: requestFields.rolloutId, workerFingerprint: requestFields.workerFingerprint,
      keeperUpdate: requestFields.keeperUpdate,
      priorPlistBase64, priorPlistMode, priorLifecycle: lifecycle, priorPid,
      priorDisabled,
      priorDurableStateVersion: durableWorkerStateVersion(), targetDurableStateVersion: null,
      createdAt: now, updatedAt: now,
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
    // The target is the release that last ran, so this is the only moment the
    // store version it migrated to is still observable; a rollback that
    // cannot restore service proves why from this record alone.
    const durableStateVersion = journal.targetDurableStateVersion ?? durableWorkerStateVersion();
    const rolling = journal.phase === "rolling-back"
      && journal.targetDurableStateVersion === durableStateVersion
      ? journal
      : writeJournal({
          ...journal, phase: "rolling-back",
          targetDurableStateVersion: durableStateVersion,
          updatedAt: new Date().toISOString(),
        });
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
