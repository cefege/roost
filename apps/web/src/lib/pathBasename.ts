// Trailing-separator-tolerant basename for a canonical worker path. Kept as a
// small public adapter because session title/workspace naming share this domain
// concept; native path semantics live exclusively in lib/nativePath.

import { workerPathBasename } from "./nativePath.ts";

export function pathBasename(path: string, workerFp = ""): string {
  return workerPathBasename(workerFp, path);
}
