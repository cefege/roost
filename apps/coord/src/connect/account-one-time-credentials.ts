// Account activation and password reset intentionally share one token grammar,
// digest, and public rejection surface. Centralizing them keeps either one-time
// credential flow from becoming a state-disclosure oracle through drift.

import { Code, ConnectError } from "@connectrpc/connect";
import { createHash } from "node:crypto";

export const ONE_TIME_TOKEN = /^[A-Za-z0-9_-]{43}$/;

export function denyRedemption(): never {
  // Token state and target-account state deliberately collapse to one result.
  throw new ConnectError("unable to complete request", Code.PermissionDenied);
}

export function invalidPassword(): never {
  throw new ConnectError("invalid password", Code.InvalidArgument);
}

export function hashToken(token: string): string {
  return createHash("sha256").update(token, "utf8").digest("hex");
}
