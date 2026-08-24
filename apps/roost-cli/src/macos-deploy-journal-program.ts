// The bun -e program transmitted by macosJournalUtilityCommand (see
// deploy-macos-journal-controller.ts) to the remote Mac, where it owns only
// durable journal/definition bytes and confined release deletion; launchd
// lifecycle transitions stay in the TypeScript recovery state machine.
// Byte-stable: recovery tooling compares transmitted commands verbatim.

export const MACOS_DEPLOY_JOURNAL_PROGRAM = String.raw`
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
const outputPrefix = "RoostMacDeployJournal=";
const shaPattern = /^[a-f0-9]{40,64}$/;
const suffixPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

function reject(message) {
  throw new Error(message);
}

function canonicalAbsolute(value, name) {
  if (!value || !path.isAbsolute(value) || path.normalize(value) !== value || /[\r\n\0]/.test(value)) {
    reject(name + " is not a canonical absolute path");
  }
  return value;
}

canonicalAbsolute(journalPath, "journal path");
canonicalAbsolute(releaseRoot, "release root");
canonicalAbsolute(plistPath, "plist path");
if (!label || /[\r\n\0/]/.test(label)) reject("launchd label is malformed");

function directReleasePath(value, sha) {
  canonicalAbsolute(value, "target release path");
  const relative = path.relative(releaseRoot, value);
  if (!relative || relative === ".." || relative.startsWith("../") || relative.includes("/")) {
    reject("target release path escapes the managed root");
  }
  if (sha !== null) {
    if (!shaPattern.test(sha)
      || !relative.startsWith(sha + "-")
      || !suffixPattern.test(relative.slice(sha.length + 1))) {
      reject("target release identity is malformed");
    }
  }
  if (fs.existsSync(releaseRoot)) {
    const rootStat = fs.lstatSync(releaseRoot);
    if (!rootStat.isDirectory() || rootStat.isSymbolicLink() || fs.realpathSync(releaseRoot) !== releaseRoot) {
      reject("managed release root is not a canonical directory");
    }
  }
  if (fs.existsSync(value)) {
    const targetStat = fs.lstatSync(value);
    if (!targetStat.isDirectory() || targetStat.isSymbolicLink() || fs.realpathSync(value) !== value) {
      reject("managed release target is not a canonical directory");
    }
  }
  return value;
}

function run(argv) {
  const result = Bun.spawnSync(argv, {
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  });
  return {
    exit: result.exitCode ?? 1,
    stdout: decoder.decode(result.stdout),
    stderr: decoder.decode(result.stderr),
  };
}

function syncDirectory(directory) {
  const fd = fs.openSync(directory, "r");
  try {
    fs.fsyncSync(fd);
  } finally {
    fs.closeSync(fd);
  }
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
    if (fd !== null) {
      try { fs.closeSync(fd); } catch {}
    }
    try { fs.rmSync(temporary, { force: true }); } catch {}
    throw error;
  }
}

function durableRemove(destination) {
  if (!fs.existsSync(destination)) return;
  fs.unlinkSync(destination);
  syncDirectory(path.dirname(destination));
}

function validateJournal(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) reject("journal is not an object");
  const keys = [
    "schemaVersion", "phase", "targetGitSha", "targetReleasePath",
    "priorPlistBase64", "priorPlistMode", "priorLifecycle", "priorPid",
    "priorDisabled", "createdAt", "updatedAt",
  ];
  if (Object.keys(value).some((key) => !keys.includes(key))) reject("journal has unknown fields");
  if (value.schemaVersion !== 1) reject("journal schema is unsupported");
  if (value.phase !== "prepared" && value.phase !== "activating") reject("journal phase is malformed");
  if (typeof value.targetGitSha !== "string" || typeof value.targetReleasePath !== "string") {
    reject("journal target identity is malformed");
  }
  directReleasePath(value.targetReleasePath, value.targetGitSha);
  if (!["unloaded", "loaded", "running"].includes(value.priorLifecycle)) {
    reject("journal prior lifecycle is malformed");
  }
  if (value.priorLifecycle === "running") {
    if (!Number.isSafeInteger(value.priorPid) || value.priorPid < 1) {
      reject("journal prior process epoch is malformed");
    }
  } else if (value.priorPid !== null) {
    reject("journal has a process epoch for a non-running service");
  }
  if (typeof value.priorDisabled !== "boolean") reject("journal disabled override is malformed");
  if (value.priorPlistBase64 === null) {
    if (value.priorPlistMode !== null || value.priorLifecycle !== "unloaded") {
      reject("journal cannot restore a loaded service without plist bytes");
    }
  } else {
    if (typeof value.priorPlistBase64 !== "string"
      || value.priorPlistBase64.length > 2 * 1024 * 1024
      || Buffer.from(value.priorPlistBase64, "base64").toString("base64") !== value.priorPlistBase64) {
      reject("journal prior plist bytes are malformed");
    }
    if (!Number.isSafeInteger(value.priorPlistMode)
      || value.priorPlistMode < 0
      || value.priorPlistMode > 0o777) {
      reject("journal prior plist mode is malformed");
    }
  }
  if (typeof value.createdAt !== "string"
    || typeof value.updatedAt !== "string"
    || !Number.isFinite(Date.parse(value.createdAt))
    || !Number.isFinite(Date.parse(value.updatedAt))) {
    reject("journal timestamps are malformed");
  }
  return value;
}

function readJournal() {
  if (!fs.existsSync(journalPath)) return null;
  const stat = fs.lstatSync(journalPath);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 3 * 1024 * 1024) {
    reject("journal file is malformed");
  }
  return validateJournal(JSON.parse(fs.readFileSync(journalPath, "utf8")));
}

function writeJournal(journal) {
  validateJournal(journal);
  durableWrite(journalPath, Buffer.from(JSON.stringify(journal) + "\n"), 0o600);
}

function emit(journal) {
  const payload = Buffer.from(JSON.stringify({ releaseRoot, journal })).toString("base64");
  console.log(outputPrefix + payload);
}

function launchdPrint() {
  const uid = String(process.getuid());
  return run(["launchctl", "print", "gui/" + uid + "/" + label]);
}


function disabledOverride() {
  const uid = String(process.getuid());
  const result = run(["launchctl", "print-disabled", "gui/" + uid]);
  if (result.exit !== 0) reject("cannot read launchd disabled overrides: " + result.stderr);
  const overrideLine = result.stdout.split(/\r?\n/)
    .find((line) => line.includes('"' + label + '"'));
  if (!overrideLine) return false;
  // launchctl prints this override in two shapes depending on the OS version:
  // the older boolean (=> true / => false) and the current word form
  // (=> disabled / => enabled, measured on macOS 15 / mihai-m5-air). Accepting
  // only the boolean made every current-macOS deploy abort as "malformed"
  // before it touched the host. Both mean the same thing: the first of each
  // pair is "this service is disabled".
  const match = overrideLine.match(/=>\s*(true|false|disabled|enabled)\s*$/);
  if (!match) reject("launchd disabled override is malformed: " + overrideLine.trim());
  return match[1] === "true" || match[1] === "disabled";
}

function exactPriorDefinition(journal) {
  if (journal.priorPlistBase64 === null) return !fs.existsSync(plistPath);
  if (!fs.existsSync(plistPath)) return false;
  const stat = fs.lstatSync(plistPath);
  return stat.isFile()
    && !stat.isSymbolicLink()
    && (stat.mode & 0o777) === journal.priorPlistMode
    && fs.readFileSync(plistPath).equals(Buffer.from(journal.priorPlistBase64, "base64"));
}

function removeRelease(releasePath, sha) {
  directReleasePath(releasePath, sha);
  if (!fs.existsSync(releasePath)) return;
  fs.rmSync(releasePath, { recursive: true, force: false });
  syncDirectory(releaseRoot);
}

function plistWorkingDirectory(bytes) {
  const temporary = journalPath + ".plist-probe-" + randomUUID();
  fs.writeFileSync(temporary, bytes, { mode: 0o600, flag: "wx" });
  try {
    const result = run([
      "/usr/libexec/PlistBuddy",
      "-c",
      "Print :WorkingDirectory",
      temporary,
    ]);
    return result.exit === 0 ? result.stdout.trim() : null;
  } finally {
    fs.rmSync(temporary, { force: true });
  }
}

try {
  if (action === "load") {
    emit(readJournal());
  } else if (action === "prepare") {
    if (fs.existsSync(journalPath)) reject("an uncleared macOS deploy journal already exists");
    directReleasePath(requestedTarget, requestedSha);
    if (!fs.existsSync(requestedTarget)) reject("target release is not staged");
    const launchd = launchdPrint();
    const lifecycle = launchd.exit !== 0
      ? "unloaded"
      : /^\s*state = running\s*$/m.test(launchd.stdout) ? "running" : "loaded";
    const priorPidMatch = launchd.stdout.match(/^\s*pid = ([1-9]\d*)\s*$/m);
    const priorPid = lifecycle === "running" && priorPidMatch
      ? Number(priorPidMatch[1])
      : null;
    if (lifecycle === "running" && !Number.isSafeInteger(priorPid)) {
      reject("cannot capture the prior worker process epoch");
    }
    let priorPlistBase64 = null;
    let priorPlistMode = null;
    if (fs.existsSync(plistPath)) {
      const stat = fs.lstatSync(plistPath);
      if (!stat.isFile() || stat.isSymbolicLink()) reject("worker plist is not a regular file");
      priorPlistBase64 = fs.readFileSync(plistPath).toString("base64");
      priorPlistMode = stat.mode & 0o777;
    } else if (lifecycle !== "unloaded") {
      reject("cannot recover a loaded worker whose plist is absent");
    }
    const now = new Date().toISOString();
    const journal = {
      schemaVersion: 1,
      phase: "prepared",
      targetGitSha: requestedSha,
      targetReleasePath: requestedTarget,
      priorPlistBase64,
      priorPlistMode,
      priorLifecycle: lifecycle,
      priorPid,
      priorDisabled: disabledOverride(),
      createdAt: now,
      updatedAt: now,
    };
    writeJournal(journal);
    emit(journal);
  } else if (action === "checkpoint-activating") {
    const journal = readJournal();
    if (!journal || journal.phase !== "prepared") reject("prepared journal is missing");
    directReleasePath(requestedTarget, requestedSha);
    if (journal.targetGitSha !== requestedSha || journal.targetReleasePath !== requestedTarget) {
      reject("activation checkpoint target does not match the prepared journal");
    }
    const activating = { ...journal, phase: "activating", updatedAt: new Date().toISOString() };
    writeJournal(activating);
    emit(activating);
  } else if (action === "restore-prior") {
    const journal = readJournal();
    if (!journal || journal.phase !== "activating") reject("activating journal is missing");
    if (journal.priorPlistBase64 === null) {
      durableRemove(plistPath);
    } else {
      durableWrite(
        plistPath,
        Buffer.from(journal.priorPlistBase64, "base64"),
        journal.priorPlistMode,
      );
    }
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
    if (!journal || journal.phase !== "activating") reject("activating journal is missing");
    if (journal.priorPlistBase64 !== null) {
      const priorPath = plistWorkingDirectory(Buffer.from(journal.priorPlistBase64, "base64"));
      if (priorPath && priorPath !== journal.targetReleasePath) {
        const protectedPaths = new Set();
        for (const name of fs.readdirSync(path.dirname(plistPath))) {
          if (!name.endsWith(".plist")) continue;
          const candidate = path.join(path.dirname(plistPath), name);
          const candidateStat = fs.lstatSync(candidate);
          if (!candidateStat.isFile() || candidateStat.isSymbolicLink()) continue;
          const result = run([
            "/usr/libexec/PlistBuddy",
            "-c",
            "Print :WorkingDirectory",
            candidate,
          ]);
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
