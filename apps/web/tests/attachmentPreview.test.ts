// attachmentPreview.test.ts — browser-local image preview MIME admission.
// Covers the selection boundary without transferring file content or paths.
// Object-URL lifetime belongs to the transfer store, not this helper.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { createAttachmentPreviewUrl } from "../src/lib/attachmentPreview.ts";

const originalCreateObjectURL = URL.createObjectURL;
let createdTypes: string[] = [];

beforeEach(() => {
  createdTypes = [];
  URL.createObjectURL = (object: Blob | MediaSource) => {
    if (!(object instanceof Blob)) throw new Error("attachment previews require Blob input");
    createdTypes.push(object.type);
    return `blob:${object.type}`;
  };
});

afterEach(() => {
  URL.createObjectURL = originalCreateObjectURL;
});

describe("createAttachmentPreviewUrl", () => {
  test("creates browser-local URLs only for supported image MIME types", () => {
    for (const type of ["image/avif", "image/gif", "image/jpeg", "image/png", "image/webp"]) {
      expect(createAttachmentPreviewUrl(new File([], "photo", { type }))).toBe(`blob:${type}`);
    }
    expect(createdTypes).toEqual(["image/avif", "image/gif", "image/jpeg", "image/png", "image/webp"]);
  });

  test("does not create URLs for unsupported image formats or non-images", () => {
    expect(createAttachmentPreviewUrl(new File([], "vector.svg", { type: "image/svg+xml" }))).toBeUndefined();
    expect(createAttachmentPreviewUrl(new File([], "notes.txt", { type: "text/plain" }))).toBeUndefined();
    expect(createdTypes).toEqual([]);
  });
});
