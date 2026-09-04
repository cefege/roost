// Defines the native-account credential policy shared by provisioning and login.
// Callers use one email normalization path and one Argon2id parameter contract.
// Keeping these limits centralized prevents authentication paths from drifting.

export const NATIVE_PASSWORD_ARGON2ID = {
  algorithm: "argon2id",
  memoryCost: 65_536,
  timeCost: 3,
} as const;

export const NATIVE_PASSWORD_MAX_LENGTH = 1_024;
export const NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH = 12;

/** Canonical account lookup key. Invalid input deliberately has no richer
 * error shape so public credential handlers cannot disclose account state. */
export function normalizeAccountEmail(raw: string): string | null {
  if (raw.length === 0 || raw.length > 320) return null;
  const email = raw.trim().toLowerCase();
  const at = email.indexOf("@");
  if (
    email.length === 0
    || email.length > 320
    || at <= 0
    || at !== email.lastIndexOf("@")
    || at === email.length - 1
    || /\s/.test(email)
  ) {
    return null;
  }
  return email;
}

export function isNativePasswordLengthValid(password: string, minimum = 1): boolean {
  return password.length >= minimum && password.length <= NATIVE_PASSWORD_MAX_LENGTH;
}
