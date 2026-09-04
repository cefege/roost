/**
 * Creates the signup gateway's root-owned credentials and deployable configuration atomically.
 * Administrative setup calls this before gateway and provisioner services are started.
 * No-follow file checks and fsync-backed replacement prevent secret disclosure or partial installs.
 */

import {
  chmodSync,
  closeSync,
  constants,
  existsSync,
  fchmodSync,
  fsyncSync,
  fstatSync,
  linkSync,
  lstatSync,
  mkdirSync,
  openSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { generateKeyPairSync, randomBytes, randomUUID } from "node:crypto";
import { basename, isAbsolute, join, resolve } from "node:path";

export const DEFAULT_SIGNUP_CREDENTIAL_DIRECTORY = "/etc/roost/saas-auth" as const;
export const SIGNUP_GENERATED_CREDENTIAL_FILES = Object.freeze({
  emailOutboxKey: "email-outbox-key",
  oauthStateKey: "oauth-state-key",
  assertionSigningKey: "assertion-signing-key",
  assertionVerifyKey: "assertion-verify-key",
} as const);

export interface SignupInitOptions {
  credentialDirectory?: string;
}

export interface SignupInitResult {
  credentialDirectory: string;
  emailOutboxKeyPath: string;
  oauthStateKeyPath: string;
  assertionSigningKeyPath: string;
  assertionVerifyKeyPath: string;
}

interface StagedFile {
  destination: string;
  temporary: string;
}

function requireRoot(): void {
  if (typeof process.geteuid !== "function" || process.geteuid() !== 0) {
    throw new Error("roost saas signup-init must run as root");
  }
}

function ensureCredentialDirectory(path: string): void {
  if (!existsSync(path)) {
    mkdirSync(path, { recursive: true, mode: 0o700 });
    chmodSync(path, 0o700);
  }
  const stat = lstatSync(path);
  if (!stat.isDirectory() || stat.isSymbolicLink()) {
    throw new Error("signup credential directory must be a real directory");
  }
  if ((stat.mode & 0o777) !== 0o700) {
    throw new Error("signup credential directory must have mode 0700");
  }
}

function uint32(value: number): Buffer {
  const bytes = Buffer.allocUnsafe(4);
  bytes.writeUInt32BE(value, 0);
  return bytes;
}

function openSshEd25519PublicKey(raw: Buffer): string {
  if (raw.byteLength !== 32) throw new Error("generated Ed25519 public key is invalid");
  const type = Buffer.from("ssh-ed25519", "ascii");
  const wire = Buffer.concat([uint32(type.byteLength), type, uint32(raw.byteLength), raw]);
  return `ssh-ed25519 ${wire.toString("base64")} roost-saas-auth\n`;
}


function createGeneratedContents(): Record<keyof typeof SIGNUP_GENERATED_CREDENTIAL_FILES, string> {
  const emailOutboxKey = `${randomBytes(32).toString("base64url")}\n`;
  const oauthStateKey = `${randomBytes(32).toString("base64url")}\n`;
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const assertionSigningKey = privateKey.export({ type: "pkcs8", format: "pem" }).toString();
  const publicJwk = publicKey.export({ format: "jwk" });
  if (typeof publicJwk.x !== "string") throw new Error("generated Ed25519 public key is invalid");
  const rawPublicKey = Buffer.from(publicJwk.x, "base64url");
  if (rawPublicKey.toString("base64url") !== publicJwk.x) {
    throw new Error("generated Ed25519 public key is invalid");
  }
  return {
    emailOutboxKey,
    oauthStateKey,
    assertionSigningKey,
    assertionVerifyKey: openSshEd25519PublicKey(rawPublicKey),
  };
}

function stageFile(
  directory: string,
  destination: string,
  content: string,
  mode: 0o600 | 0o644,
): StagedFile {
  const temporary = join(directory, `.${basename(destination)}.${randomUUID()}.tmp`);
  const fd = openSync(
    temporary,
    constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW,
    mode,
  );
  try {
    writeFileSync(fd, content, "utf8");
    fchmodSync(fd, mode);
    fsyncSync(fd);
    const stat = fstatSync(fd);
    if (!stat.isFile() || (stat.mode & 0o777) !== mode) {
      throw new Error("could not create signup credential with required mode");
    }
  } finally {
    closeSync(fd);
  }
  return { destination, temporary };
}

function unlinkIfPresent(path: string): void {
  try {
    unlinkSync(path);
  } catch (error) {
    if (!(error instanceof Error) || !("code" in error) || error.code !== "ENOENT") throw error;
  }
}

/**
 * Creates the gateway-generated credentials. Each final name is installed by an
 * exclusive hard link, so an existing credential is never replaced, even if it
 * appears after the preflight check. No secret value is returned or printed.
 */
export function runSignupInit(options: SignupInitOptions = {}): SignupInitResult {
  requireRoot();
  const directoryInput = options.credentialDirectory ?? DEFAULT_SIGNUP_CREDENTIAL_DIRECTORY;
  if (!isAbsolute(directoryInput)) throw new Error("signup credential directory must be absolute");
  const credentialDirectory = resolve(directoryInput);
  ensureCredentialDirectory(credentialDirectory);

  const result: SignupInitResult = {
    credentialDirectory,
    emailOutboxKeyPath: join(credentialDirectory, SIGNUP_GENERATED_CREDENTIAL_FILES.emailOutboxKey),
    oauthStateKeyPath: join(credentialDirectory, SIGNUP_GENERATED_CREDENTIAL_FILES.oauthStateKey),
    assertionSigningKeyPath: join(credentialDirectory, SIGNUP_GENERATED_CREDENTIAL_FILES.assertionSigningKey),
    assertionVerifyKeyPath: join(credentialDirectory, SIGNUP_GENERATED_CREDENTIAL_FILES.assertionVerifyKey),
  };
  const destinations = [
    result.emailOutboxKeyPath,
    result.oauthStateKeyPath,
    result.assertionSigningKeyPath,
    result.assertionVerifyKeyPath,
  ];
  for (const destination of destinations) {
    if (existsSync(destination)) throw new Error(`refusing to overwrite signup credential: ${destination}`);
  }

  const contents = createGeneratedContents();
  const staged: StagedFile[] = [];
  const installed: string[] = [];
  try {
    staged.push(
      stageFile(credentialDirectory, result.emailOutboxKeyPath, contents.emailOutboxKey, 0o600),
      stageFile(credentialDirectory, result.oauthStateKeyPath, contents.oauthStateKey, 0o600),
      stageFile(credentialDirectory, result.assertionSigningKeyPath, contents.assertionSigningKey, 0o600),
      // This file is a public trust anchor bind-mounted into a 65532:65532
      // container. It contains no secret and must be readable by that process.
      stageFile(credentialDirectory, result.assertionVerifyKeyPath, contents.assertionVerifyKey, 0o644),
    );
    for (const file of staged) {
      linkSync(file.temporary, file.destination);
      installed.push(file.destination);
    }
    const directoryFd = openSync(credentialDirectory, constants.O_RDONLY | constants.O_DIRECTORY);
    try {
      fsyncSync(directoryFd);
    } finally {
      closeSync(directoryFd);
    }
  } catch (error) {
    for (const destination of installed.reverse()) unlinkIfPresent(destination);
    throw error;
  } finally {
    for (const file of staged) unlinkIfPresent(file.temporary);
  }
  return result;
}
