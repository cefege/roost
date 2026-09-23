// Real loopback-door coverage for the attachment-specific WebSocket route.
// It proves the distinct subprotocol reaches only attachment handlers while
// retaining the terminal door's existing Host and Origin admission boundary.

import { afterEach, expect, test } from "bun:test";
import {
  ATTACHMENT_TRANSFER_LOOPBACK_PATH,
  ATTACHMENT_TRANSFER_LOOPBACK_MAX_PAYLOAD_BYTES,
  ATTACHMENT_TRANSFER_LOOPBACK_SUBPROTOCOL,
} from "@roost/shared/attachment-transfer";
import { startLocalUiServer, type LocalUiServer } from "../src/local-ui-server.ts";
import type { AttachmentTransferPort } from "../src/attachment-transfer-port.ts";

const started: LocalUiServer[] = [];

afterEach(() => {
  for (const server of started.splice(0)) server.close();
});

test("attachment loopback route uses its own subprotocol and handler", async () => {
  let opened: AttachmentTransferPort | undefined;
  let resolveOpened: ((port: AttachmentTransferPort) => void) | undefined;
  const openedPromise = new Promise<AttachmentTransferPort>((resolve) => { resolveOpened = resolve; });
  let resolveFrame: ((bytes: Uint8Array) => void) | undefined;
  const framePromise = new Promise<Uint8Array>((resolve) => { resolveFrame = resolve; });
  const server = startLocalUiServer({
    bind: "127.0.0.1:0",
    coordinatorUrl: "http://coord.test:4102",
    workerFingerprint: "a".repeat(64),
    allowedBrowserOrigins: [],
    spa: async () => new Response("spa"),
    terminal: { onOpen: () => undefined, onMessage: () => undefined, onClose: () => undefined },
    attachment: {
      onOpen: (port) => {
        opened = port;
        resolveOpened?.(port);
      },
      onMessage: (_port, bytes) => { resolveFrame?.(new Uint8Array(bytes)); },
      onClose: () => undefined,
    },
  });
  started.push(server);
  const origin = `http://127.0.0.1:${server.port}`;
  expect((await fetch(`${origin}${ATTACHMENT_TRANSFER_LOOPBACK_PATH}`)).status).toBe(400);

  const client = new WebSocket(
    `ws://127.0.0.1:${server.port}${ATTACHMENT_TRANSFER_LOOPBACK_PATH}`,
    [ATTACHMENT_TRANSFER_LOOPBACK_SUBPROTOCOL],
  );
  client.binaryType = "arraybuffer";
  await new Promise<void>((resolve, reject) => {
    client.onopen = () => resolve();
    client.onerror = () => reject(new Error("attachment loopback socket refused"));
  });
  const port = await openedPromise;
  const received = new Promise<Uint8Array>((resolve) => { client.onmessage = (event) => resolve(new Uint8Array(event.data as ArrayBuffer)); });
  client.send(Uint8Array.of(1, 2, 3));
  expect(await framePromise).toEqual(Uint8Array.of(1, 2, 3));
  expect(port.send(Uint8Array.of(9, 8), "control")).toBe("accepted");
  expect(await received).toEqual(Uint8Array.of(9, 8));
  expect(opened?.kind).toBe("loopback");
  expect(client.protocol).toBe(ATTACHMENT_TRANSFER_LOOPBACK_SUBPROTOCOL);
  client.close();
});

test("attachment loopback refuses an oversized frame before handler decoding", async () => {
  let messages = 0;
  const server = startLocalUiServer({
    bind: "127.0.0.1:0",
    coordinatorUrl: "http://coord.test:4102",
    workerFingerprint: "a".repeat(64),
    allowedBrowserOrigins: [],
    spa: async () => new Response("spa"),
    terminal: { onOpen: () => undefined, onMessage: () => undefined, onClose: () => undefined },
    attachment: {
      onOpen: () => undefined,
      onMessage: () => { messages += 1; },
      onClose: () => undefined,
    },
  });
  started.push(server);
  const client = new WebSocket(
    `ws://127.0.0.1:${server.port}${ATTACHMENT_TRANSFER_LOOPBACK_PATH}`,
    [ATTACHMENT_TRANSFER_LOOPBACK_SUBPROTOCOL],
  );
  await new Promise<void>((resolve, reject) => {
    client.onopen = () => resolve();
    client.onerror = () => reject(new Error("attachment loopback socket refused"));
  });
  const closed = new Promise<void>((resolve) => { client.onclose = () => resolve(); });
  client.send(new Uint8Array(ATTACHMENT_TRANSFER_LOOPBACK_MAX_PAYLOAD_BYTES + 1));
  await closed;
  expect(messages).toBe(0);
});
