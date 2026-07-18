// Version string baked into the compiled binary via `bun build --define`
// (scripts/build-binary.ts stamps package.json version + short git sha). From
// source there's no define, so `typeof` is "undefined" → "dev". `roost update`
// and the release-drift check compare against this.
declare const __ROOST_VERSION__: string | undefined;

export const ROOST_VERSION: string =
  typeof __ROOST_VERSION__ === "string" ? __ROOST_VERSION__ : "dev";

export function version(_args: string[]): void {
  console.log(ROOST_VERSION);
}
