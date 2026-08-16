import type { SupportedHostPlatform } from "@roost/shared/platform";

const WINDOWS_JOIN_URL = "https://github.com/cefege/roost/releases/latest/download/join.ps1";
const POSIX_JOIN_URL = "https://raw.githubusercontent.com/cefege/roost/main/join.sh";

function powerShellLiteral(value: string): string {
  return `'${value.replace(/'/g, "''")}'`;
}

export function machinePlatformLabel(platform: SupportedHostPlatform): string {
  switch (platform) {
    case "darwin": return "Mac";
    case "linux": return "Linux machine";
    case "win32": return "Windows machine";
  }
}

export function buildMachineJoinCommand(
  platform: SupportedHostPlatform,
  coordinatorUrl: string,
  bootstrapToken: string,
  workerLabel: string,
): string {
  if (platform !== "win32") {
    const labelEnv = workerLabel ? ` ROOST_WORKER_LABEL=${JSON.stringify(workerLabel)}` : "";
    return `curl -fsSL ${POSIX_JOIN_URL} | `
      + `ROOST_COORDINATOR_URL=${JSON.stringify(coordinatorUrl)} `
      + `ROOST_BOOTSTRAP_TOKEN=${JSON.stringify(bootstrapToken)}${labelEnv} bash`;
  }

  const labelArg = workerLabel ? ` -WorkerLabel ${powerShellLiteral(workerLabel)}` : "";
  return `$p=Join-Path $env:TEMP 'roost-join.ps1'; `
    + `Invoke-WebRequest '${WINDOWS_JOIN_URL}' -OutFile $p; `
    + `if((Get-AuthenticodeSignature $p).Status -ne 'Valid'){throw 'Invalid Roost signature'}; `
    + `& $p -CoordinatorUrl ${powerShellLiteral(coordinatorUrl)} `
    + `-BootstrapToken ${powerShellLiteral(bootstrapToken)}${labelArg}`;
}
