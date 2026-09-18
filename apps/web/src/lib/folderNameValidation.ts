// Client-side validation for the picker's "New folder" name, so an invalid
// name is named inline before any mkdir RPC leaves the browser. Returns the
// exact message the dialog renders; first failing check wins.
//
// Callers: WorkerBrowsePage.tsx (commitNewFolder).

export function validateNewFolderName(
  name: string,
  siblingNames: readonly string[],
): { ok: true } | { ok: false; message: string } {
  const trimmed = name.trim();
  if (trimmed === "") return { ok: false, message: "Enter a folder name." };
  if (/[/\\]/.test(name)) return { ok: false, message: "Folder names can't contain / or \\." };
  if (/[\u0000-\u001f]/.test(name)) {
    return { ok: false, message: "Folder names can't contain control characters." };
  }
  // Ordered before the trailing-period check so `..` reports the dot-name
  // message instead of the less specific "can't end with a period".
  if (trimmed === "." || trimmed === "..") {
    return { ok: false, message: "Choose a name other than . or .." };
  }
  if (/[ .]$/.test(trimmed)) {
    return { ok: false, message: "Folder names can't end with a space or period." };
  }
  if (trimmed.length > 255) {
    return { ok: false, message: "Folder names must be 255 characters or fewer." };
  }
  const lower = trimmed.toLowerCase();
  if (siblingNames.some((sibling) => sibling.toLowerCase() === lower)) {
    return { ok: false, message: `A folder named "${trimmed}" already exists here.` };
  }
  return { ok: true };
}
