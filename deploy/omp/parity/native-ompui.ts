import { randomUUID } from "node:crypto";
import type { Page, Request } from "playwright";

/**
 * The native-contract-conformance lane's protocol channel. It never invents a
 * session: every identity field, header and open parameter is copied from the
 * real browser client's latest `AgentOmpUi` request, and the action is sent by
 * the page itself through the same routed, recorded transport.
 */
export const agentOmpUiPath = "/roost.v1.CoordinatorService/AgentOmpUi";
const forwardedHeaders = ["authorization", "content-type", "connect-protocol-version", "x-roost-tab-id"] as const;

interface OmpUiRequestFields { conversationId: string; requestId: string; actionJson: string; viewId: string; tabId: string; transportId: string; receipt: boolean }
interface OmpUiIdentity { url: string; headers: Record<string, string>; conversationId: string; viewId: string; tabId: string; transportId: string; open?: Record<string, unknown> }

export class NativeOmpUiChannel {
  #identity: OmpUiIdentity | undefined;

  /** Called for every real routed `AgentOmpUi` request before it is forwarded. */
  async observe(request: Request): Promise<void> {
    const headers = await request.allHeaders();
    if (headers["content-encoding"]) throw new Error(`Compressed AgentOmpUi request bodies are not decodable: ${headers["content-encoding"]}`);
    if (headers["content-type"] !== "application/proto") throw new Error(`Unexpected AgentOmpUi content type: ${headers["content-type"]}`);
    const body = request.postDataBuffer();
    if (!body) throw new Error("AgentOmpUi request carried no body");
    const fields = decodeRequest(body);
    const action = JSON.parse(fields.actionJson) as { method?: unknown; params?: unknown };
    const forwarded: Record<string, string> = {};
    for (const name of forwardedHeaders) {
      const value = headers[name];
      if (value === undefined) throw new Error(`AgentOmpUi request is missing header ${name}`);
      forwarded[name] = value;
    }
    const previous = this.#identity;
    const sameView = previous?.conversationId === fields.conversationId && previous.viewId === fields.viewId;
    this.#identity = {
      url: request.url(),
      headers: forwarded,
      conversationId: fields.conversationId,
      viewId: fields.viewId,
      tabId: fields.tabId,
      // An open names no transport yet; the view keeps the one its later events carry.
      transportId: fields.transportId || (sameView ? previous.transportId : ""),
      open: action.method === "open" && typeof action.params === "object" && action.params !== null && !Array.isArray(action.params)
        ? action.params as Record<string, unknown>
        : sameView ? previous.open : undefined,
    };
  }

  /** A native event addressed to the real node the selector resolves to. */
  async sendEvent(page: Page, target: { surface: string; node: string }, event: Record<string, unknown>): Promise<void> {
    if (typeof event.ev !== "string" || !event.ev) throw new Error("A native event needs its `ev` verb");
    await this.#send(page, { method: "event", params: { event: { ...event, sf: target.surface, id: target.node } } });
  }

  /** Refreshes the live transport with the client's own open parameters and a changed feature set. */
  async refreshOpen(page: Page, features: readonly string[]): Promise<void> {
    const open = this.#identity?.open;
    if (!open) throw new Error("No real AgentOmpUi open was observed for this view");
    await this.#send(page, { method: "open", params: { ...open, features: [...features] } });
  }

  async #send(page: Page, action: Record<string, unknown>): Promise<void> {
    const identity = this.#identity;
    if (!identity?.transportId) throw new Error("No real AgentOmpUi transport identity was observed yet");
    const body = encodeRequest({
      conversationId: identity.conversationId,
      requestId: randomUUID(),
      actionJson: JSON.stringify(action),
      viewId: identity.viewId,
      tabId: identity.tabId,
      transportId: identity.transportId,
      receipt: false,
    });
    const result = await page.evaluate(async ({ url, headers, bytes }) => {
      const response = await fetch(url, { method: "POST", headers, body: new Uint8Array(bytes) });
      return { status: response.status, text: response.ok ? "" : await response.text() };
    }, { url: identity.url, headers: identity.headers, bytes: [...body] });
    if (result.status !== 200) throw new Error(`AgentOmpUi ${String(action.method)} was rejected (${result.status}): ${result.text}`);
  }
}

function readVarint(bytes: Uint8Array, offset: number): [number, number] {
  let value = 0;
  let scale = 1;
  for (let index = offset; index < bytes.length && index < offset + 8; index++) {
    const byte = bytes[index]!;
    value += (byte & 0x7f) * scale;
    if (byte < 0x80) return [value, index + 1];
    scale *= 128;
  }
  throw new Error("Malformed protobuf varint in AgentOmpUi request");
}

function decodeRequest(bytes: Uint8Array): OmpUiRequestFields {
  const fields: OmpUiRequestFields = { conversationId: "", requestId: "", actionJson: "", viewId: "", tabId: "", transportId: "", receipt: false };
  const names = ["", "conversationId", "requestId", "actionJson", "viewId", "tabId", "transportId"] as const;
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let offset = 0;
  while (offset < bytes.length) {
    const [tag, afterTag] = readVarint(bytes, offset);
    const field = Math.floor(tag / 8);
    const wire = tag % 8;
    if (wire === 0) {
      const [value, next] = readVarint(bytes, afterTag);
      if (field === 7) fields.receipt = value !== 0;
      offset = next;
    } else if (wire === 2) {
      const [length, start] = readVarint(bytes, afterTag);
      const end = start + length;
      if (end > bytes.length) throw new Error("Truncated AgentOmpUi request field");
      const name = field >= 1 && field <= 6 ? names[field] : undefined;
      if (name) fields[name] = decoder.decode(bytes.subarray(start, end));
      offset = end;
    } else throw new Error(`Unexpected protobuf wire type ${wire} in AgentOmpUi request`);
  }
  return fields;
}

function encodeVarint(value: number, out: number[]): void {
  let rest = value;
  while (rest >= 0x80) {
    out.push((rest % 128) | 0x80);
    rest = Math.floor(rest / 128);
  }
  out.push(rest);
}

function encodeRequest(fields: OmpUiRequestFields): Uint8Array {
  const out: number[] = [];
  const encoder = new TextEncoder();
  const strings: Array<[number, string]> = [[1, fields.conversationId], [2, fields.requestId], [3, fields.actionJson], [4, fields.viewId], [5, fields.tabId], [6, fields.transportId]];
  for (const [field, value] of strings) {
    if (!value) continue;
    const bytes = encoder.encode(value);
    encodeVarint(field * 8 + 2, out);
    encodeVarint(bytes.length, out);
    for (const byte of bytes) out.push(byte);
  }
  if (fields.receipt) out.push(7 * 8, 1);
  return Uint8Array.from(out);
}
