// Protected Windows TLS installation copies explicit credentials into service state.
// Quickstart records prior bytes and ACLs before publishing either file.
// Rollback restores both contents and security without deleting concurrent data.

import { randomUUID } from "node:crypto";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  rmdirSync,
} from "node:fs";
import { basename, dirname, join } from "node:path";
import {
  durableRemove,
  durableReplace,
  durableWriteFile,
  flushDurablePath,
} from "@roost/shared/durability";
import { windowsApplyArtifactDacl } from "@roost/shared/windows-helper";
import {
  protectWindowsRoleStateTree,
  restoreWindowsFileSecurityTree,
  snapshotWindowsFileSecurityTree,
  type WindowsFileSecurityTreeSnapshot,
} from "./install-binary-agents.ts";
import {
  requireResolvedEndpoint,
  type QuickstartEndpoint,
} from "./quickstart-endpoint.ts";
import {
  requireCanonicalWindowsPath,
  type CoordinatorPaths,
} from "./quickstart-windows-state.ts";

interface WindowsTlsTargetBaseline {
  path: string;
  contents: Buffer | null;
}

export interface WindowsTlsInstallRollback {
  tlsDir: string;
  tlsDirExisted: boolean;
  security: WindowsFileSecurityTreeSnapshot | null;
  targets: readonly [WindowsTlsTargetBaseline, WindowsTlsTargetBaseline];
  helperPath: string;
  writerAccount: string;
}

async function publishProtectedWindowsTlsFile(
  path: string,
  contents: Uint8Array,
  helperPath: string,
  writerAccount: string,
): Promise<void> {
  const temporary = join(dirname(path), `.${basename(path)}.${process.pid}.${randomUUID()}.tmp`);
  try {
    await durableWriteFile(temporary, contents, { platform: "win32" });
    await windowsApplyArtifactDacl(temporary, writerAccount, { helperPath });
    await durableReplace(temporary, path, { platform: "win32" });
    await windowsApplyArtifactDacl(path, writerAccount, { helperPath });
    await flushDurablePath(path, { platform: "win32" });
    await flushDurablePath(dirname(path), { platform: "win32" });
  } finally {
    await durableRemove(temporary, { platform: "win32" }).catch(() => undefined);
  }
}

export async function rollbackWindowsTlsInstall(state: WindowsTlsInstallRollback): Promise<void> {
  for (const target of state.targets) {
    if (target.contents) {
      await publishProtectedWindowsTlsFile(
        target.path,
        target.contents,
        state.helperPath,
        state.writerAccount,
      );
    } else {
      await durableRemove(target.path, { platform: "win32" });
    }
  }
  if (state.security) {
    await restoreWindowsFileSecurityTree(state.security, { helperPath: state.helperPath });
  } else if (!state.tlsDirExisted && existsSync(state.tlsDir)) {
    try {
      rmdirSync(state.tlsDir);
    } catch {
      // The two transaction-owned files were removed above. Keep a non-empty
      // directory rather than recursively deleting unexpected concurrent data.
    }
  }
  await flushDurablePath(dirname(state.tlsDir), { platform: "win32" });
  for (const target of state.targets) target.contents?.fill(0);
}

export async function prepareWindowsTlsInstall(
  endpoint: QuickstartEndpoint,
  paths: CoordinatorPaths,
  account: string,
  interactiveSid: string,
): Promise<{ state: WindowsTlsInstallRollback; certPath: string; keyPath: string }> {
  requireResolvedEndpoint(endpoint);
  if (endpoint.mode !== "explicit") {
    throw new Error("protected Windows TLS copies are only valid for explicit mode");
  }
  const helperPath = requireCanonicalWindowsPath("ROOST_WIN_HELPER");
  const certPath = join(paths.tlsDir, "quickstart-explicit.crt");
  const keyPath = join(paths.tlsDir, "quickstart-explicit.key");
  const tlsDirExisted = existsSync(paths.tlsDir);
  const security = tlsDirExisted
    ? await snapshotWindowsFileSecurityTree(paths.tlsDir, { helperPath })
    : null;
  const capture = (path: string): WindowsTlsTargetBaseline => {
    if (!existsSync(path)) return { path, contents: null };
    const info = lstatSync(path);
    if (!info.isFile() || info.isSymbolicLink()) {
      throw new Error("existing coordinator TLS target is not a regular non-reparse file");
    }
    return { path, contents: readFileSync(path) };
  };
  const targets = [capture(certPath), capture(keyPath)] as const;
  const state: WindowsTlsInstallRollback = {
    tlsDir: paths.tlsDir,
    tlsDirExisted,
    security,
    targets,
    helperPath,
    writerAccount: account,
  };
  const certContents = readFileSync(endpoint.tlsCertPath);
  const keyContents = readFileSync(endpoint.tlsKeyPath);
  try {
    mkdirSync(paths.tlsDir, { recursive: true });
    await publishProtectedWindowsTlsFile(certPath, certContents, helperPath, account);
    await publishProtectedWindowsTlsFile(keyPath, keyContents, helperPath, account);
    await protectWindowsRoleStateTree(paths.tlsDir, "coordinator-state", {
      account,
      interactiveSid,
      helperPath,
    });
    return { state, certPath, keyPath };
  } catch (error) {
    try {
      await rollbackWindowsTlsInstall(state);
    } catch (rollbackError) {
      throw new Error(`Windows TLS copy failed and rollback was incomplete: ${String(rollbackError)}`);
    }
    throw error;
  } finally {
    certContents.fill(0);
    keyContents.fill(0);
  }
}

export function commitWindowsTlsInstall(state: WindowsTlsInstallRollback | null): void {
  if (!state) return;
  for (const target of state.targets) target.contents?.fill(0);
}
