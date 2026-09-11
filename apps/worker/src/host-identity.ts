// Collects static machine display identity once per worker process. Registration
// and heartbeat reuse this cache so a worker reports one coherent host record.
// Sources are local sysctl, os-release, and Windows CIM; serials never enter
// this module or the public Worker record.

import { execFileSync } from "node:child_process";
import { closeSync, openSync, readSync } from "node:fs";
import {
  assertNeverPlatform,
  supportedHostPlatform,
  type SupportedHostPlatform,
} from "@roost/shared/platform";
import {
  normalizeHostIdentity,
  normalizeHostIdentityText,
  type HostIdentity,
} from "@roost/shared/wire";

const HOST_IDENTITY_SOURCE_MAX_BYTES = 8 * 1024;
const SYSCTL_TIMEOUT_MS = 1_000;
const APPLE_CHIP = /^Apple M\d+(?: (?:Pro|Max|Ultra))?$/;

export interface HostIdentitySources {
  readDarwinSysctl(name: string): string | null;
  readLinuxOsRelease(): string | null;
  readWindowsModel(): string | null;
}

function readDarwinSysctl(name: string): string | null {
  try {
    return execFileSync("/usr/sbin/sysctl", ["-n", name], {
      encoding: "utf8",
      timeout: SYSCTL_TIMEOUT_MS,
      maxBuffer: HOST_IDENTITY_SOURCE_MAX_BYTES,
    });
  } catch {
    return null;
  }
}

function readLinuxOsRelease(): string | null {
  let descriptor: number | null = null;
  try {
    descriptor = openSync("/etc/os-release", "r");
    const buffer = Buffer.allocUnsafe(HOST_IDENTITY_SOURCE_MAX_BYTES);
    const bytesRead = readSync(
      descriptor,
      buffer,
      0,
      HOST_IDENTITY_SOURCE_MAX_BYTES,
      0,
    );
    return buffer.subarray(0, bytesRead).toString("utf8");
  } catch {
    return null;
  } finally {
    if (descriptor !== null) {
      try {
        closeSync(descriptor);
      } catch {
        // The read result remains usable if a descriptor was concurrently closed.
      }
    }
  }
}

function readWindowsModel(): string | null {
  try {
    return execFileSync("powershell.exe", [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "(Get-CimInstance -ClassName Win32_ComputerSystem).Model",
    ], {
      encoding: "utf8",
      timeout: SYSCTL_TIMEOUT_MS,
      maxBuffer: HOST_IDENTITY_SOURCE_MAX_BYTES,
      windowsHide: true,
    });
  } catch {
    return null;
  }
}

const DEFAULT_HOST_IDENTITY_SOURCES: HostIdentitySources = {
  readDarwinSysctl,
  readLinuxOsRelease,
  readWindowsModel,
};

function osReleaseValue(source: string, key: string): string | null {
  for (const line of source.split(/\r?\n/)) {
    if (!line.startsWith(`${key}=`)) continue;
    const rawValue = line.slice(key.length + 1).trim();
    if (rawValue.length < 2 || rawValue[0] !== '"' || rawValue.at(-1) !== '"') {
      return rawValue;
    }

    let value = "";
    for (let index = 1; index < rawValue.length - 1; index += 1) {
      const character = rawValue[index]!;
      if (character !== "\\" || index + 1 >= rawValue.length - 1) {
        value += character;
        continue;
      }
      const escaped = rawValue[index + 1]!;
      value += escaped === "\\" || escaped === '"' || escaped === "$" || escaped === "`"
        ? escaped
        : `\\${escaped}`;
      index += 1;
    }
    return value;
  }
  return null;
}

export function collectHostIdentity(
  platform: SupportedHostPlatform = supportedHostPlatform(),
  sources: HostIdentitySources = DEFAULT_HOST_IDENTITY_SOURCES,
): HostIdentity | null {
  switch (platform) {
    case "darwin": {
      const chip = normalizeHostIdentityText(
        sources.readDarwinSysctl("machdep.cpu.brand_string"),
      );
      return normalizeHostIdentity({
        hardware_model: sources.readDarwinSysctl("hw.model"),
        chip: chip !== null && APPLE_CHIP.test(chip) ? chip : null,
        linux_distribution: null,
      });
    }
    case "linux": {
      const source = sources.readLinuxOsRelease();
      const linuxDistribution = source === null
        ? null
        : normalizeHostIdentityText(osReleaseValue(source, "PRETTY_NAME"))
          ?? osReleaseValue(source, "NAME");
      return normalizeHostIdentity({
        hardware_model: null,
        chip: null,
        linux_distribution: linuxDistribution,
      });
    }
    case "win32":
      return normalizeHostIdentity({
        hardware_model: sources.readWindowsModel(),
        chip: null,
        linux_distribution: null,
      });
    default:
      return assertNeverPlatform(platform);
  }
}

let cachedHostIdentity: HostIdentity | null | undefined;

/** Returns the process-static identity shared by registration and heartbeat. */
export function staticHostIdentity(): HostIdentity | null {
  if (cachedHostIdentity === undefined) cachedHostIdentity = collectHostIdentity();
  return cachedHostIdentity;
}
