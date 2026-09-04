// Creates and verifies the per-instance filesystem layout for managed coordinators.
// Docker seeding and runtime adoption use these paths for data and mounted secrets.
// Ownership, modes, and atomic writes protect tenant credentials across retries.
import { MANAGED_WEB_PUBLIC_ORIGIN } from "@roost/shared/config";
import {
  chmodSync,
  chownSync,
  closeSync,
  constants,
  existsSync,
  fchmodSync,
  fchownSync,
  fstatSync,
  fsyncSync,
  lstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  renameSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { randomBytes, randomUUID } from "node:crypto";
import type { RegistryAccount, RegistryCoordinator } from "./registry.ts";

const SECRET_MAX_BYTES = 64 * 1024;
const OUTBOX_KEY_RE = /^[A-Za-z0-9_-]{43}$/;

export interface InstanceLayout {
  instanceDir: string;
  dataDir: string;
  secretsDir: string;
  authorizedKeysPath: string;
  verifierDir: string;
  manifestPath: string;
  resendApiKeyPath: string;
  authVerifyKeyPath: string;
  outboxKeyPath: string;
  coordinatorKeyPath: string;
}

export interface EnsureInstanceLayoutOptions {
  sharedResendApiKeyPath: string;
  sharedAuthVerifyKeyPath: string;
  uid?: number;
  gid?: number;
  randomKey?: () => Uint8Array;
}

function checkedRegularFile(
  path: string,
  mode: number,
  uid: number,
  gid: number,
  maxBytes: number,
  validContent: (content: string) => boolean,
): string {
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = fstatSync(fd);
    if (!stat.isFile()) throw new Error(`refusing non-regular instance file: ${path}`);
    if ((stat.mode & 0o777) !== mode) throw new Error(`instance file has wrong mode: ${path}`);
    if (stat.size < 0 || stat.size > maxBytes) throw new Error(`instance file is oversized: ${path}`);
    const content = readFileSync(fd, "utf8");
    if (!validContent(content)) throw new Error(`instance file content is invalid: ${path}`);
    if (stat.uid !== uid || stat.gid !== gid) {
      if (stat.uid !== 0 || stat.gid !== 0) throw new Error(`instance file has wrong owner: ${path}`);
      fchownSync(fd, uid, gid);
    }
    return content;
  } finally {
    closeSync(fd);
  }
}

function checkedDirectory(path: string, uid: number, gid: number): void {
  const stat = lstatSync(path);
  if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`refusing non-directory instance path: ${path}`);
  if ((stat.mode & 0o777) !== 0o700) throw new Error(`instance directory has wrong mode: ${path}`);
  if (stat.uid !== uid || stat.gid !== gid) {
    if (stat.uid !== 0 || stat.gid !== 0) throw new Error(`instance directory has wrong owner: ${path}`);
    chownSync(path, uid, gid);
  }
}

function ensureDirectory(path: string, uid: number, gid: number): void {
  if (!existsSync(path)) {
    mkdirSync(path, { recursive: true, mode: 0o700 });
    chmodSync(path, 0o700);
    chownSync(path, uid, gid);
  }
  checkedDirectory(path, uid, gid);
}

function createExactFile(path: string, content: string, uid: number, gid: number): void {
  const temporary = join(dirname(path), `.${basename(path)}.${randomUUID()}.tmp`);
  const fd = openSync(
    temporary,
    constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW,
    0o600,
  );
  try {
    writeFileSync(fd, content, "utf8");
    fchmodSync(fd, 0o600);
    fchownSync(fd, uid, gid);
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
  renameSync(temporary, path);
  const directoryFd = openSync(dirname(path), constants.O_RDONLY | constants.O_DIRECTORY);
  try { fsyncSync(directoryFd); } finally { closeSync(directoryFd); }
}

function readBoundedSecret(path: string, name: string): string {
  const stat = lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size <= 0 || stat.size > SECRET_MAX_BYTES) {
    throw new Error(`${name} file is invalid`);
  }
  const value = readFileSync(path, "utf8").trim();
  if (!value || Buffer.byteLength(value, "utf8") > SECRET_MAX_BYTES) {
    throw new Error(`${name} file is invalid`);
  }
  return value;
}

export function instanceLayoutFor(coordinator: RegistryCoordinator): InstanceLayout {
  const dataDir = resolve(coordinator.dataDir);
  const instanceDir = dirname(dataDir);
  if (dataDir !== join(instanceDir, "data")) throw new Error("coordinator data path is not canonical");
  const secretsDir = join(instanceDir, "secrets");
  return {
    instanceDir,
    dataDir,
    secretsDir,
    authorizedKeysPath: join(dataDir, "authorized_keys.roost"),
    manifestPath: join(dataDir, "instance.json"),
    resendApiKeyPath: join(secretsDir, "resend-api-key"),
    authVerifyKeyPath: join(instanceDir, "verifier", "saas-auth-verify-key"),
    verifierDir: join(instanceDir, "verifier"),
    outboxKeyPath: join(secretsDir, "email-outbox-key"),
    coordinatorKeyPath: join(secretsDir, "ssh_ed25519.key"),
  };
}

export function expectedInstanceManifest(
  account: RegistryAccount,
  coordinator: RegistryCoordinator,
): string {
  return `${JSON.stringify({
    version: 2,
    account_id: account.id,
    coordinator_id: coordinator.id,
    ordinal: coordinator.ordinal,
    route_key: coordinator.routeKey,
    dashboard_url: MANAGED_WEB_PUBLIC_ORIGIN,
    container_name: coordinator.containerName,
  })}\n`;
}

export function ensureInstanceLayout(
  account: RegistryAccount,
  coordinator: RegistryCoordinator,
  options: EnsureInstanceLayoutOptions,
): InstanceLayout {
  if (coordinator.accountId !== account.id) throw new Error("coordinator account mismatch");
  const uid = options.uid ?? 65_532;
  const gid = options.gid ?? 65_532;
  if (!Number.isInteger(uid) || uid < 0 || !Number.isInteger(gid) || gid < 0) {
    throw new Error("invalid managed runtime owner");
  }
  const layout = instanceLayoutFor(coordinator);
  ensureDirectory(layout.instanceDir, uid, gid);
  ensureDirectory(layout.dataDir, uid, gid);
  ensureDirectory(layout.verifierDir, uid, gid);
  ensureDirectory(layout.secretsDir, uid, gid);

  const manifest = expectedInstanceManifest(account, coordinator);
  if (!existsSync(layout.manifestPath)) createExactFile(layout.manifestPath, manifest, uid, gid);
  checkedRegularFile(layout.manifestPath, 0o600, uid, gid, Buffer.byteLength(manifest), (content) =>
    content === manifest
  );

  if (!existsSync(layout.authorizedKeysPath)) createExactFile(layout.authorizedKeysPath, "", uid, gid);
  checkedRegularFile(layout.authorizedKeysPath, 0o600, uid, gid, 1024 * 1024, () => true);

  const resendKey = readBoundedSecret(options.sharedResendApiKeyPath, "shared Resend API key");
  if (!existsSync(layout.resendApiKeyPath)) createExactFile(layout.resendApiKeyPath, resendKey, uid, gid);
  checkedRegularFile(layout.resendApiKeyPath, 0o600, uid, gid, SECRET_MAX_BYTES, (content) =>
    content === resendKey
  );

  const authVerifyKey = readBoundedSecret(options.sharedAuthVerifyKeyPath, "shared SaaS auth verify key");
  if (!existsSync(layout.authVerifyKeyPath)) createExactFile(layout.authVerifyKeyPath, authVerifyKey, uid, gid);
  checkedRegularFile(layout.authVerifyKeyPath, 0o600, uid, gid, SECRET_MAX_BYTES, (content) =>
    content === authVerifyKey
  );

  if (!existsSync(layout.outboxKeyPath)) {
    const bytes = (options.randomKey ?? (() => randomBytes(32)))();
    if (bytes.byteLength !== 32) throw new Error("outbox key generator returned the wrong size");
    createExactFile(layout.outboxKeyPath, Buffer.from(bytes).toString("base64url"), uid, gid);
  }
  checkedRegularFile(layout.outboxKeyPath, 0o600, uid, gid, 64, (content) =>
    OUTBOX_KEY_RE.test(content)
  );

  if (existsSync(layout.coordinatorKeyPath)) {
    checkedRegularFile(layout.coordinatorKeyPath, 0o600, uid, gid, SECRET_MAX_BYTES, (content) =>
      content.includes("OPENSSH PRIVATE KEY")
    );
  }
  return layout;
}
