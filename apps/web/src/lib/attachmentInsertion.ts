// Attachment paths become terminal input, so unsafe control characters must not reach the shell.
// Composer insertion calls this boundary after an upload returns its worker-side path.
// Host platform rules and the shared shell quoter determine whether that path is insertable.

import type { SupportedHostPlatform } from "@roost/shared/platform";
import { posixShellQuote } from "@roost/shared/shell-quote";

const CONTROL_CHARACTER_RE = /[\u0000-\u001f\u007f-\u009f]/u;

export function safeAttachmentInsertion(
  platform: SupportedHostPlatform,
  absPath: string,
): string | null {
  if (CONTROL_CHARACTER_RE.test(absPath)) return null;
  return platform === "darwin" || platform === "linux"
    ? posixShellQuote(absPath)
    : null;
}
