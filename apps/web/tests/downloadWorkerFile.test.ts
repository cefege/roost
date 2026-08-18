// File href parsing must invert the single browser-native path codec exactly;
// otherwise terminal links fetch a different worker path than they display.

import { describe, test, expect, mock } from "bun:test";
import { workerFileHref } from "../src/lib/nativePath.ts";

let responseData = new Uint8Array();
const filesReadChunk = mock(async () => ({
  size: BigInt(responseData.length),
  data: responseData,
  eof: true,
}));
const addToast = mock((_message: string, _kind: string) => {});
const addTransfer = mock((_transfer: unknown) => {});
const markTransferState = mock((_id: string, _state: string, _message?: string) => {});
const setTransferProgress = mock((_id: string, _offset: number, _total: number) => {});
const sanitize = mock((html: string) => html.replace(/<script\b[^>]*>[\s\S]*?<\/script>/gi, ""));

mock.module("../src/connect.ts", () => ({ coordClient: { filesReadChunk } }));
mock.module("../src/store/toastStore.ts", () => ({ addToast }));
mock.module("../src/store/transfers.ts", () => ({ addTransfer, markTransferState, setTransferProgress }));
mock.module("dompurify", () => ({ default: { sanitize } }));

// Dynamic import keeps production dependencies behind the mocks above.
const { downloadWorkerFileByHref, parseFileHref } = await import("../src/lib/downloadWorkerFile.ts");

type DownloadCapture = {
  readonly blob: Blob | null;
  readonly filename: string;
  restore: () => void;
};

function captureDownload(): DownloadCapture {
  const documentDescriptor = Object.getOwnPropertyDescriptor(globalThis, "document");
  const createObjectURL = URL.createObjectURL;
  const revokeObjectURL = URL.revokeObjectURL;
  let blob: Blob | null = null;
  let filename = "";

  Object.defineProperty(globalThis, "document", {
    configurable: true,
    value: {
      body: { appendChild: () => {} },
      createElement: () => {
        const anchor = {
          href: "",
          download: "",
          click: () => { filename = anchor.download; },
          remove: () => {},
        };
        return anchor;
      },
    },
  });
  URL.createObjectURL = (value: Blob) => {
    blob = value;
    return "blob:test-download";
  };
  URL.revokeObjectURL = () => {};

  return {
    get blob() { return blob; },
    get filename() { return filename; },
    restore: () => {
      if (documentDescriptor) Object.defineProperty(globalThis, "document", documentDescriptor);
      else Reflect.deleteProperty(globalThis, "document");
      URL.createObjectURL = createObjectURL;
      URL.revokeObjectURL = revokeObjectURL;
    },
  };
}

function buildHref(workerFp: string, abs: string, line?: number): string {
  return workerFileHref(workerFp, abs, line ?? null);
}

describe("parseFileHref inverts resolveFile", () => {
  test("plain absolute path", () => {
    expect(parseFileHref(buildHref("abc123", "/Users/you/x.ts"))).toEqual({
      workerFp: "abc123", path: "/Users/you/x.ts",
    });
  });

  test("strips the #L<line> fragment", () => {
    expect(parseFileHref(buildHref("abc123", "/repo/apps/web/foo.ts", 42))).toEqual({
      workerFp: "abc123", path: "/repo/apps/web/foo.ts",
    });
  });

  test("round-trips segments needing URL encoding (spaces, #, %)", () => {
    const abs = "/Users/me/my docs/a#b%c.ts";
    expect(parseFileHref(buildHref("fp", abs))).toEqual({ workerFp: "fp", path: abs });
  });
  test("round-trips Windows drive and UNC paths", () => {
    for (const path of ["C:/Users/Ada/src/main.ts", "//server/share/My Folder/readme.md"]) {
      expect(parseFileHref(buildHref("winfp", path))).toEqual({ workerFp: "winfp", path });
    }
  });

  test("non-file href → null (not a terminal file link)", () => {
    expect(parseFileHref("/s/some-session-id")).toBeNull();
    expect(parseFileHref("https://example.com/x")).toBeNull();
  });

  test("malformed path encoding fails closed", () => {
    expect(parseFileHref("/file/abc123/bad%zz.md")).toBeNull();
  });
});

describe("downloadWorkerFileByHref", () => {
  test("renders an uppercase .MD file as a sanitized standalone HTML download", async () => {
    responseData = new TextEncoder().encode("# Title\n<script>alert('unsafe')</script>");
    filesReadChunk.mockClear();
    sanitize.mockClear();
    const download = captureDownload();

    try {
      await downloadWorkerFileByHref(buildHref("abc123", "/repo/README.MD"));

      expect(download.filename).toBe("README.html");
      expect(download.blob).not.toBeNull();
      expect(download.blob?.type).toBe("text/html;charset=utf-8");
      expect(filesReadChunk).toHaveBeenCalledWith({
        workerFp: "abc123", path: "/repo/README.MD", offset: 0n, len: 4 * 1024 * 1024,
      });
      expect(sanitize).toHaveBeenCalledTimes(1);
      expect(sanitize.mock.calls[0][0]).toContain("<h1>Title</h1>");
      expect(sanitize.mock.calls[0][0]).toContain("<script>alert('unsafe')</script>");
      expect(await download.blob?.text()).toBe(
        `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"></head><body><h1>Title</h1>\n</body></html>`,
      );
    } finally {
      download.restore();
    }
  });

  test("keeps a non-terminal .md suffix as an untyped raw download", async () => {
    const markdown = "# Title\n<script>alert('unsafe')</script>";
    responseData = new TextEncoder().encode(markdown);
    filesReadChunk.mockClear();
    sanitize.mockClear();
    const download = captureDownload();

    try {
      await downloadWorkerFileByHref(buildHref("abc123", "/repo/notes.md.bak"));

      expect(download.filename).toBe("notes.md.bak");
      expect(download.blob).not.toBeNull();
      expect(download.blob?.type).toBe("");
      expect(await download.blob?.text()).toBe(markdown);
      expect(sanitize).not.toHaveBeenCalled();
    } finally {
      download.restore();
    }
  });
});
