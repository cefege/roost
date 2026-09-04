// `roost organizations bootstrap-owner` provisions the initial managed owner
// directly in a migrated local coordinator database.
// Self-hosted startup owns its tenant automatically; this path stays managed-only.

import { Database } from "bun:sqlite";
import { existsSync } from "node:fs";
import { randomUUID } from "node:crypto";
import { join } from "node:path";
import { coordDataDir } from "@roost/shared/paths";
import {
  isNativePasswordLengthValid,
  NATIVE_PASSWORD_ARGON2ID,
  NATIVE_PASSWORD_MAX_LENGTH,
  NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH,
  normalizeAccountEmail,
} from "@roost/shared/native-credentials";
import {
  assertBootstrapSchema,
  assertUnassignedOwnerRuntimeState,
  BOOTSTRAP_OWNER_RUNTIME_SCOPE_TABLES,
  countBootstrapRows,
  MUST_BE_EMPTY,
  type BootstrapOwnerRuntimeScopeTable,
} from "./organizations-bootstrap-database.ts";

const SLUG_RE = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const MAX_SLUG_LENGTH = 63;

export interface BootstrapOwnerInput {
  email: string;
  organization: string;
  dashboard: string;
}

export interface BootstrapOwnerOptions {
  databasePath: string;
  input: BootstrapOwnerInput;
  password: string;
  now?: () => number;
  createId?: () => string;
}

export interface BootstrapOwnerResult {
  accountId: string;
  ownerEmailNormalized: string;
  organizationId: string;
  dashboardId: string;
  assignments: Record<BootstrapOwnerRuntimeScopeTable, number>;
}

export interface OwnerBootstrapPasswordSource {
  environment?: Record<string, string | undefined>;
  readStdin?: () => Promise<Uint8Array>;
}

export const OWNER_BOOTSTRAP_PASSWORD_ENV = "ROOST_OWNER_BOOTSTRAP_PASSWORD";


function ownerUsageError(message: string): Error {
  return new Error(
    `${message}. Usage: roost organizations bootstrap-owner ` +
      "--email <address> --organization <slug> --dashboard <slug> " +
      `(password from stdin or ${OWNER_BOOTSTRAP_PASSWORD_ENV})`,
  );
}


function validateSlug(
  raw: string,
  flag: "--organization" | "--dashboard",
): string {
  if (
    raw !== raw.trim()
    || raw !== raw.toLowerCase()
    || raw.length === 0
    || raw.length > MAX_SLUG_LENGTH
    || !SLUG_RE.test(raw)
  ) {
    throw ownerUsageError(`${flag} must be a lowercase DNS-style slug`);
  }
  return raw;
}


/** Parse the managed bootstrap shape. Password material is intentionally not
 * representable in argv; even a future unknown `--password` flag is rejected. */
export function parseBootstrapOwnerCommand(args: readonly string[]): BootstrapOwnerInput {
  if (args[0] !== "bootstrap-owner") {
    throw ownerUsageError("organizations requires the bootstrap-owner subcommand");
  }

  const values: Partial<Record<"--email" | "--organization" | "--dashboard", string>> = {};
  const expected: Record<"--email" | "--organization" | "--dashboard", true> = {
    "--email": true,
    "--organization": true,
    "--dashboard": true,
  };
  for (let index = 1; index < args.length; index += 1) {
    const flag = args[index];
    if (typeof flag !== "string" || !(flag in expected)) {
      throw ownerUsageError(`unknown organizations argument: ${flag}`);
    }
    const namedFlag = flag as "--email" | "--organization" | "--dashboard";
    if (values[namedFlag] !== undefined) {
      throw ownerUsageError(`duplicate organizations argument: ${flag}`);
    }
    const value = args[index + 1];
    if (!value || value.startsWith("--")) throw ownerUsageError(`missing value for ${flag}`);
    values[namedFlag] = value;
    index += 1;
  }

  const email = values["--email"];
  const organization = values["--organization"];
  const dashboard = values["--dashboard"];
  if (!email || !organization || !dashboard) {
    throw ownerUsageError("--email, --organization, and --dashboard are all required");
  }
  const emailNormalized = normalizeAccountEmail(email);
  if (!emailNormalized) throw ownerUsageError("--email must be a valid account email");
  return {
    email: emailNormalized,
    organization: validateSlug(organization, "--organization"),
    dashboard: validateSlug(dashboard, "--dashboard"),
  };
}

function validatedOwnerPassword(raw: string, stripTerminalNewline: boolean): string {
  let password = raw;
  if (stripTerminalNewline) {
    password = password.endsWith("\r\n")
      ? password.slice(0, -2)
      : password.endsWith("\n")
        ? password.slice(0, -1)
        : password;
  }
  if (
    /[\0\r\n]/.test(password)
    || !isNativePasswordLengthValid(password, NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH)
  ) {
    throw new Error(
      `owner password must be ${NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH}-${NATIVE_PASSWORD_MAX_LENGTH} characters with no NUL or newline`,
    );
  }
  return password;
}

/** Read the initial owner password without accepting it through argv or disk.
 * The protected environment value is deleted immediately so child processes
 * cannot inherit it; stdin bytes are overwritten after decoding. */
export async function readOwnerBootstrapPassword(
  source: OwnerBootstrapPasswordSource = {},
): Promise<string> {
  const environment = source.environment ?? process.env;
  const environmentPassword = environment[OWNER_BOOTSTRAP_PASSWORD_ENV];
  if (environmentPassword !== undefined) {
    delete environment[OWNER_BOOTSTRAP_PASSWORD_ENV];
    return validatedOwnerPassword(environmentPassword, false);
  }

  if (!source.readStdin && process.stdin.isTTY) {
    throw new Error(
      `owner password is required through piped stdin or ${OWNER_BOOTSTRAP_PASSWORD_ENV}; interactive echoed input is refused`,
    );
  }
  const bytes = source.readStdin
    ? await source.readStdin()
    : new Uint8Array(await Bun.file(0).arrayBuffer());
  try {
    if (bytes.byteLength > 4_096) throw new Error("owner password input is too large");
    const raw = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    return validatedOwnerPassword(raw, true);
  } finally {
    bytes.fill(0);
  }
}

function defaultCoordinatorDbPath(): string {
  const dataDir = process.env.ROOST_COORD_DATA_DIR ?? coordDataDir();
  return process.env.ROOST_COORDINATOR_DB ?? join(dataDir, "coordinator_v2.db");
}



/**
 * Provision the one managed owner, organization, and dashboard directly in a
 * migrated local coordinator database. The password is Argon2id-hashed before
 * the SQLite write lock, then every pristine-state check and mutation shares
 * one BEGIN IMMEDIATE transaction.
 */
export async function bootstrapOwner(options: BootstrapOwnerOptions): Promise<BootstrapOwnerResult> {
  if (!existsSync(options.databasePath)) {
    throw new Error(`local coordinator database does not exist: ${options.databasePath}`);
  }
  const emailNormalized = normalizeAccountEmail(options.input.email);
  if (!emailNormalized) throw new Error("owner email is invalid");
  const organization = validateSlug(options.input.organization, "--organization");
  const dashboard = validateSlug(options.input.dashboard, "--dashboard");
  const password = validatedOwnerPassword(options.password, false);
  const passwordHash = await Bun.password.hash(password, NATIVE_PASSWORD_ARGON2ID);
  const now = options.now ?? Date.now;
  const createId = options.createId ?? randomUUID;
  const sqlite = new Database(options.databasePath);
  let transactionOpen = false;

  try {
    sqlite.exec("PRAGMA foreign_keys = ON");
    sqlite.exec("PRAGMA busy_timeout = 5000");
    assertBootstrapSchema(sqlite);
    sqlite.exec("BEGIN IMMEDIATE");
    transactionOpen = true;

    if (countBootstrapRows(sqlite, "accounts") !== 0) {
      throw new Error("refusing bootstrap-owner: accounts already exist");
    }
    for (const table of MUST_BE_EMPTY) {
      if (countBootstrapRows(sqlite, table) !== 0) {
        throw new Error(`refusing bootstrap-owner: unsafe existing state in ${table}`);
      }
    }
    assertUnassignedOwnerRuntimeState(sqlite);

    const timestamp = now();
    if (!Number.isSafeInteger(timestamp) || timestamp < 0) {
      throw new Error("bootstrap-owner clock must return a non-negative integer millisecond timestamp");
    }
    const accountId = createId();
    const organizationId = createId();
    const dashboardId = createId();
    if (!accountId || !organizationId || !dashboardId) {
      throw new Error("bootstrap-owner ID generator returned an empty ID");
    }

    sqlite.query(
      `INSERT INTO accounts
        (id, email_normalized, password_hash, status, created_at_ms, password_changed_at_ms)
       VALUES (?, ?, ?, 'active', ?, ?)`,
    ).run(accountId, emailNormalized, passwordHash, timestamp, timestamp);
    sqlite.query(
      `INSERT INTO account_identities (
         account_id, issuer, subject, email_normalized, linked_at_ms,
         last_authenticated_at_ms, revoked_at_ms
       ) VALUES (?, 'native', ?, ?, ?, NULL, NULL)`,
    ).run(accountId, accountId, emailNormalized, timestamp);
    sqlite.query(
      "INSERT INTO organizations (id, slug, name, status, created_at_ms) VALUES (?, ?, ?, 'active', ?)",
    ).run(organizationId, organization, organization, timestamp);
    sqlite.query(
      "INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms) VALUES (?, ?, ?, ?, 'active', ?)",
    ).run(dashboardId, organizationId, dashboard, dashboard, timestamp);
    sqlite.query(
      "INSERT INTO organization_memberships (organization_id, account_id, role, created_at_ms) VALUES (?, ?, 'owner', ?)",
    ).run(organizationId, accountId, timestamp);
    sqlite.query(
      "INSERT INTO dashboard_memberships (dashboard_id, account_id, role, created_at_ms) VALUES (?, ?, 'admin', ?)",
    ).run(dashboardId, accountId, timestamp);

    const assignments = {} as Record<BootstrapOwnerRuntimeScopeTable, number>;
    for (const table of BOOTSTRAP_OWNER_RUNTIME_SCOPE_TABLES) {
      const statement = sqlite.prepare(`UPDATE ${table} SET dashboard_id = ?`);
      try {
        const result = statement.run(dashboardId);
        assignments[table] = result.changes;
      } finally {
        statement.finalize();
      }
    }

    sqlite.exec("COMMIT");
    transactionOpen = false;
    return {
      accountId,
      ownerEmailNormalized: emailNormalized,
      organizationId,
      dashboardId,
      assignments,
    };
  } catch (error) {
    if (transactionOpen) {
      try {
        sqlite.exec("ROLLBACK");
      } catch {
        // Preserve the original bootstrap failure.
      }
    }
    throw error;
  } finally {
    sqlite.close(true);
  }
}

export async function organizations(args: string[]): Promise<void> {
  const input = parseBootstrapOwnerCommand(args);
  let password = await readOwnerBootstrapPassword();
  try {
    const result = await bootstrapOwner({
      databasePath: defaultCoordinatorDbPath(),
      input,
      password,
    });
    console.log(JSON.stringify({
      event: "organizations.bootstrap_owner.complete",
      organization: input.organization,
      dashboard: input.dashboard,
      account_id: result.accountId,
      organization_id: result.organizationId,
      dashboard_id: result.dashboardId,
      assignments: result.assignments,
    }));
  } finally {
    password = "";
  }
}
