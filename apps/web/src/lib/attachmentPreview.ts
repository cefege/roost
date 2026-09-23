// Attachment preview URLs — creates browser-local image thumbnails for transfer rows.
// Upload selection owns construction; the transfer store owns each URL's lifetime.
// Only explicit image MIME types can produce a preview, never names or file paths.

const PREVIEWABLE_IMAGE_TYPES: Record<string, true> = {
  "image/avif": true,
  "image/gif": true,
  "image/jpeg": true,
  "image/png": true,
  "image/webp": true,
};

export function createAttachmentPreviewUrl(file: File): string | undefined {
  return PREVIEWABLE_IMAGE_TYPES[file.type] === true ? URL.createObjectURL(file) : undefined;
}
