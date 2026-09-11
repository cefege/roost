// Machine identity presentation maps the static Worker record into safe sidebar
// chrome. Folder rows consume this pure projection; it never inspects labels,
// session metadata, or volatile presence fields.
// Linux marks remain browser-local so the sidebar has no runtime asset dependency.

import type { SupportedHostPlatform } from "@roost/shared/platform";

export type LinuxDistributionBrand =
  | "alpine"
  | "arch"
  | "debian"
  | "fedora"
  | "ubuntu";

export interface MachineHostIdentity {
  hardware_model: string | null;
  chip: string | null;
  linux_distribution: string | null;
}

export interface MachineIdentityInput {
  os: SupportedHostPlatform | null;
  host_identity?: MachineHostIdentity | null;
}
export interface MachineIdentityPresentation {
  icon: string;
  label: string;
  title: string;
  appleChipBadge: string | null;
  linuxBrand: LinuxDistributionBrand | null;
}

interface LinuxDistributionPresentation {
  brand: LinuxDistributionBrand;
  label: string;
}

const LINUX_DISTRIBUTIONS: readonly (readonly [RegExp, LinuxDistributionPresentation])[] = [
  [/^Alpine Linux(?:\s|$)/i, { brand: "alpine", label: "Alpine Linux" }],
  [/^Arch Linux(?:\s|$)/i, { brand: "arch", label: "Arch Linux" }],
  [/^Debian(?: GNU\/Linux)?(?:\s|$)/i, { brand: "debian", label: "Debian Linux" }],
  [/^Fedora(?: Linux)?(?:\s|$)/i, { brand: "fedora", label: "Fedora Linux" }],
  [/^Ubuntu(?:\s|$)/i, { brand: "ubuntu", label: "Ubuntu Linux" }],
];

const MACBOOK_HARDWARE_MODEL_IDS = [
  "Mac14,2", "Mac14,5", "Mac14,6", "Mac14,7", "Mac14,9", "Mac14,10", "Mac14,15",
  "Mac15,3", "Mac15,6", "Mac15,7", "Mac15,8", "Mac15,9", "Mac15,10", "Mac15,11",
  "Mac15,12", "Mac15,13",
] as const;

const WINDOWS_LAPTOP_MODEL = /\b(?:laptop|notebook|book)\b/i;

export function machineIdentityPresentation(input: MachineIdentityInput | undefined): MachineIdentityPresentation {
  const os = input?.os ?? null;
  const hostIdentity = input?.host_identity ?? null;

  switch (os) {
    case "darwin": {
      const isMacBook = isMacBookHardware(hostIdentity?.hardware_model ?? null);
      const appleChipBadge = appleChipBadgeFor(hostIdentity?.chip ?? null);
      const label = isMacBook ? "MacBook" : "Mac";
      return {
        icon: isMacBook ? "laptop_mac" : "desktop_mac",
        label,
        title: appleChipBadge ? `${label} · Apple ${appleChipBadge}` : label,
        appleChipBadge,
        linuxBrand: null,
      };
    }
    case "linux": {
      const linuxDistribution = linuxDistributionFor(hostIdentity?.linux_distribution ?? null);
      const label = linuxDistribution?.label ?? "Linux";
      return {
        icon: "dns",
        label,
        title: `${label} machine`,
        appleChipBadge: null,
        linuxBrand: linuxDistribution?.brand ?? null,
      };
    }
    case "win32": {
      const isLaptop = WINDOWS_LAPTOP_MODEL.test(hostIdentity?.hardware_model ?? "");
      return genericMachinePresentation(
        isLaptop ? "laptop_windows" : "desktop_windows",
        isLaptop ? "Windows laptop" : "Windows PC",
      );
    }
    case null:
      return genericMachinePresentation("computer", "Computer");
  }
}

function genericMachinePresentation(icon: string, label: string): MachineIdentityPresentation {
  return { icon, label, title: label, appleChipBadge: null, linuxBrand: null };
}

function appleChipBadgeFor(chip: string | null): string | null {
  if (!chip) return null;
  const match = /^Apple\s+(M[1-9]\d?(?:\s+(?:Pro|Max|Ultra))?)$/i.exec(chip.trim());
  return match ? match[1].replace(/^m/, "M") : null;
}

function linuxDistributionFor(distribution: string | null): LinuxDistributionPresentation | null {
  if (!distribution) return null;
  const name = distribution.trim();
  for (const [pattern, presentation] of LINUX_DISTRIBUTIONS) {
    if (pattern.test(name)) return presentation;
  }
  return null;
}

function isMacBookHardware(hardwareModel: string | null): boolean {
  if (!hardwareModel) return false;
  if (hardwareModel.startsWith("MacBook")) return true;
  return MACBOOK_HARDWARE_MODEL_IDS.some((modelId) => modelId === hardwareModel);
}
