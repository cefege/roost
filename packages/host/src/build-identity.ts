// Compile-time identity for self-contained Roost binaries. Source processes
// fall back to their service-provided commit stamp; compiled processes always
// report the artifact's own immutable identity after an in-place update.
declare const __ROOST_VERSION__: string | undefined;
declare const __ROOST_GIT_SHA__: string | undefined;

export const ROOST_ARTIFACT_VERSION =
  typeof __ROOST_VERSION__ === "string" ? __ROOST_VERSION__ : "dev";
export const ROOST_BUILD_SHA =
  typeof __ROOST_GIT_SHA__ === "string"
    ? __ROOST_GIT_SHA__
    : process.env.GIT_SHA ?? process.env.ROOST_GIT_SHA ?? "dev";
export const IS_COMPILED_ROOST_BUILD =
  typeof __ROOST_GIT_SHA__ === "string";
