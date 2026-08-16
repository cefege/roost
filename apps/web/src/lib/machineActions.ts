// OS-specific machine actions shared by folder/session context menus. A worker
// advertises what it can serve: macOS exposes Finder/VNC, Windows exposes an
// RDP connection file plus a native UNC share path, and Linux exposes neither.

import type { SupportedHostPlatform } from "@roost/shared/platform";

export type MachineActionId = "finder" | "screen-share" | "remote-desktop" | "network-share";
export type MachineActionOperation = "navigate" | "download-rdp" | "copy-unc";

export interface MachineActionDefinition {
  id: MachineActionId;
  label: string;
  operation: MachineActionOperation;
}
export function machinePlatformIcon(platform: SupportedHostPlatform | null): string {
  switch (platform) {
    case "darwin": return "desktop_mac";
    case "linux": return "dns";
    case "win32": return "desktop_windows";
    case null: return "computer";
  }
}

export function machineActionsForWorker(
  platform: SupportedHostPlatform,
  includeFiles: boolean,
): readonly MachineActionDefinition[] {
  switch (platform) {
    case "darwin":
      return [
        ...(includeFiles ? [{ id: "finder", label: "Open in Finder", operation: "navigate" } as const] : []),
        { id: "screen-share", label: "Screen sharing", operation: "navigate" },
      ];
    case "linux":
      return [];
    case "win32":
      return [
        ...(includeFiles ? [{ id: "network-share", label: "Copy network share path", operation: "copy-unc" } as const] : []),
        { id: "remote-desktop", label: "Remote Desktop", operation: "download-rdp" },
      ];
  }
}

function checkedHost(host: string): string {
  const value = host.trim();
  if (!value || /[\r\n\\/]/.test(value)) throw new Error("Invalid machine address");
  return value;
}

export function machineActionHref(id: MachineActionId, host: string): string | null {
  const safeHost = checkedHost(host);
  switch (id) {
    case "finder": return `smb://${safeHost}`;
    case "screen-share": return `vnc://${safeHost}`;
    case "remote-desktop":
    case "network-share":
      return null;
  }
}

export function windowsSharePath(host: string): string {
  return `\\\\${checkedHost(host)}\\`;
}

export function remoteDesktopFile(host: string): string {
  return [
    `full address:s:${checkedHost(host)}`,
    "prompt for credentials:i:1",
    "authentication level:i:2",
    "redirectclipboard:i:1",
    "",
  ].join("\r\n");
}

export async function invokeMachineAction(action: MachineActionDefinition, host: string): Promise<void> {
  if (action.operation === "navigate") {
    const href = machineActionHref(action.id, host);
    if (href) window.location.href = href;
    return;
  }
  if (action.operation === "copy-unc") {
    await navigator.clipboard.writeText(windowsSharePath(host));
    return;
  }

  const safeHost = checkedHost(host);
  const blob = new Blob([remoteDesktopFile(safeHost)], { type: "application/x-rdp" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = `${safeHost.replace(/[^A-Za-z0-9._-]+/g, "-")}.rdp`;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
