import type { SupportedHostPlatform } from "./platform.ts";

const WINDOWS_RELEASE_API = "https://api.github.com/repos/cefege/roost/releases/latest";
const POSIX_JOIN_URL = "https://raw.githubusercontent.com/cefege/roost/main/join.sh";

function powerShellLiteral(value: string): string {
  return `'${value.replace(/'/g, "''")}'`;
}

function windowsPublisherPin(value: string | undefined): string {
  const normalized = value?.trim().toLowerCase() ?? "";
  if (!/^[0-9a-f]{64}$/.test(normalized)) {
    throw new Error("Windows enrollment requires the trusted 64-hex release-publisher SHA-256");
  }
  return normalized;
}

export function machinePlatformLabel(platform: SupportedHostPlatform): string {
  switch (platform) {
    case "darwin": return "Mac";
    case "linux": return "Linux machine";
    case "win32": return "Windows machine";
  }
}

/**
 * Build a copy-paste enrollment command. Windows verifies the downloaded
 * bootstrap's Authenticode chain, timestamp, and independently supplied leaf
 * certificate pin before PowerShell executes any downloaded code.
 */
export function buildMachineJoinCommand(
  platform: SupportedHostPlatform,
  coordinatorUrl: string,
  bootstrapToken: string,
  workerLabel: string,
  windowsPublisherSha256?: string,
): string {
  if (platform !== "win32") {
    const labelEnv = workerLabel ? ` ROOST_WORKER_LABEL=${JSON.stringify(workerLabel)}` : "";
    return `curl -fsSL ${POSIX_JOIN_URL} | `
      + `ROOST_COORDINATOR_URL=${JSON.stringify(coordinatorUrl)} `
      + `ROOST_BOOTSTRAP_TOKEN=${JSON.stringify(bootstrapToken)}${labelEnv} bash`;
  }

  const publisher = windowsPublisherPin(windowsPublisherSha256);
  const labelArg = workerLabel ? ` -WorkerLabel ${powerShellLiteral(workerLabel)}` : "";
  return `$h=${powerShellLiteral(publisher)}; `
    + `$q=[IO.Path]::GetFullPath($env:ProgramData); `
    + `if(((Get-Item -LiteralPath $q -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)-ne 0){throw 'Unsafe ProgramData directory'}; `
    + `$o=([Security.Principal.NTAccount](Get-Acl -LiteralPath $q).Owner).Translate([Security.Principal.SecurityIdentifier]).Value; `
    + `if($o -notin @('S-1-5-18','S-1-5-32-544')){throw 'ProgramData owner is not trusted'}; `
    + `$x=Join-Path $q ('RoostBootstrap-'+[Guid]::NewGuid().ToString('N')); try{ `
    + `$z=[Security.AccessControl.DirectorySecurity]::new(); `
    + `$z.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)'); `
    + `[IO.Directory]::CreateDirectory($x,$z)|Out-Null; `
    + `& icacls.exe $x '/setintegritylevel' '(OI)(CI)H'|Out-Null; `
    + `if($LASTEXITCODE -ne 0){throw 'Cannot protect Roost bootstrap staging directory'}; `
    + `$r=Invoke-RestMethod ${powerShellLiteral(WINDOWS_RELEASE_API)}; `
    + `$b="https://github.com/cefege/roost/releases/download/$($r.tag_name)"; `
    + `$p=Join-Path $x 'join.ps1'; `
    + `Invoke-WebRequest -UseBasicParsing "$b/join.ps1" -OutFile $p; `
    + `$s=Get-AuthenticodeSignature -LiteralPath $p; `
    + `if($s.Status -ne 'Valid' -or $null -eq $s.SignerCertificate -or $null -eq $s.TimeStamperCertificate){throw 'Invalid Roost signature or timestamp'}; `
    + `$d=[Security.Cryptography.SHA256]::Create(); `
    + `try{$a=(-join ($d.ComputeHash($s.SignerCertificate.RawData)|ForEach-Object{$_.ToString('x2')}))}finally{$d.Dispose()}; `
    + `if($a -cne $h){throw 'Unexpected Roost publisher'}; `
    + `Set-ExecutionPolicy -Scope Process -ExecutionPolicy RemoteSigned -Force; `
    + `$t=ConvertTo-SecureString ${powerShellLiteral(bootstrapToken)} -AsPlainText -Force; `
    + `& $p -CoordinatorUrl ${powerShellLiteral(coordinatorUrl)} `
    + `-BootstrapToken $t -PublisherSha256 $h -ReleaseBaseUrl $b${labelArg}}finally{$t=$null; Remove-Item -LiteralPath $x -Recurse -Force -ErrorAction SilentlyContinue}`;
}
