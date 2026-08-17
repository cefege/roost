// Version/build identity baked into compiled binaries. Source mode remains
// "dev" for release comparisons while service health uses the source SHA.
import { ROOST_ARTIFACT_VERSION, ROOST_BUILD_SHA } from "@roost/shared/build-identity";

export const ROOST_VERSION = ROOST_ARTIFACT_VERSION;

export function version(args: string[]): void {
  if (args.length === 0) {
    console.log(ROOST_VERSION);
    return;
  }
  if (args.length === 1 && args[0] === "--build") {
    console.log(ROOST_BUILD_SHA);
    return;
  }
  throw new Error("usage: roost version [--build]");
}
