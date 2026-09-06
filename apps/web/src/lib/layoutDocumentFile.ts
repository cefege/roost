// Browser-only file boundary for portable layout documents.
// Export stays local through Blob downloads, while import reads one user-picked
// JSON file without invoking attachment upload or any coordinator transport.

import type { LayoutDocumentV1 } from "@roost/shared/layout-document";

const DOWNLOAD_NAME = "roost-layout-v1.json";
const REVOKE_DELAY_MS = 1_000;

export function serializeLayoutDocument(document: LayoutDocumentV1): string {
  return `${JSON.stringify(document, null, 2)}\n`;
}

export function downloadLayoutDocument(document: LayoutDocumentV1): void {
  const blob = new Blob([serializeLayoutDocument(document)], {
    type: "application/json;charset=utf-8",
  });
  const url = URL.createObjectURL(blob);
  const anchor = window.document.createElement("a");
  anchor.href = url;
  anchor.download = DOWNLOAD_NAME;
  anchor.hidden = true;
  window.document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), REVOKE_DELAY_MS);
}

/** Open a single local JSON picker from a user gesture. The callback owns all
 * reading and validation; this adapter always removes its transient input. */
export function pickLayoutDocumentFile(onSelected: (file: File) => void): void {
  const input = window.document.createElement("input");
  input.type = "file";
  input.accept = ".json,application/json";
  input.multiple = false;
  input.hidden = true;
  input.setAttribute("data-testid", "layout-document-file-input");
  let cleaned = false;
  const cleanup = () => {
    if (cleaned) return;
    cleaned = true;
    input.remove();
  };
  input.onchange = () => {
    try {
      const file = input.files?.[0];
      if (file) onSelected(file);
    } finally {
      cleanup();
    }
  };
  input.oncancel = cleanup;
  window.document.body.appendChild(input);
  input.click();
}
