// Windows/POSIX service-control public surface. The implementation lives in
// focused modules re-exported here: service-posix.ts (launchd/systemd command
// strings — byte-stability-sensitive), windows-service-types.ts (SCM
// vocabulary), windows-service-definitions.ts (topology build + persistence),
// windows-service-scm.ts (sc.exe primitives), windows-service-security.ts
// (SID/DACL provisioning), and windows-service-manager.ts (public manager).
//
// Every CLI entry point, deploy/update flow, and test imports this barrel;
// extracted modules must import siblings directly, never this file.

export * from "./service-posix.ts";
export * from "./windows/windows-service-types.ts";
export * from "./windows/windows-service-definitions.ts";
export * from "./windows/windows-service-manager.ts";
