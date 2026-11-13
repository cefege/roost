import type { RuntimeWorkflowBoundary } from "./builtin-runtime-workflow-requirements";
import { runtimeWorkflowTraceEnvironment } from "./builtin-runtime-workflow-requirements";
import type { CaptureState } from "./builtin-capture-state";
import { objectValue, readBoundaryRows } from "./builtin-capture-state";

type ProviderRequest = Extract<RuntimeWorkflowBoundary, { channel: "runtime.provider.request" }>["value"];

export async function runtimeProviderRequests(state: CaptureState): Promise<ProviderRequest[]> {
  const requests: ProviderRequest[] = [];
  for (const row of await readBoundaryRows(state, runtimeWorkflowTraceEnvironment)) {
    if (row.channel !== "runtime.provider.request") continue;
    const request = objectValue(row.value, "Actual runtime provider request");
    if (typeof request.requestId !== "string" || typeof request.modelId !== "string" || typeof request.url !== "string" || typeof request.stream !== "boolean" || !Object.hasOwn(request, "body")) throw new Error("Invalid actual runtime provider request boundary");
    requests.push({ requestId: request.requestId, modelId: request.modelId, url: request.url, body: request.body, stream: request.stream });
  }
  return requests;
}

export function providerMessages(request: ProviderRequest): Record<string, unknown>[] {
  const body = objectValue(request.body, "Actual provider body");
  if (!Array.isArray(body.messages)) throw new Error("Actual runtime provider body lacks messages");
  return body.messages.map(message => objectValue(message, "Actual provider message"));
}

export function wireText(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) throw new Error("Actual message content is not text or blocks");
  return content.flatMap(block => {
    const value = objectValue(block, "Actual content block");
    return value.type === "text" && typeof value.text === "string" ? [value.text] : [];
  }).join("\n");
}

export function pngPayload(data: string): Buffer {
  const payload = Buffer.from(data, "base64");
  if (payload.length <= 8 || !payload.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))) throw new Error("Actual snapcompact frame is not a nonempty PNG payload");
  return payload;
}

export function imagePayloads(value: unknown): Buffer[] {
  if (Array.isArray(value)) return value.flatMap(imagePayloads);
  if (value === null || typeof value !== "object") return [];
  const object = objectValue(value, "Actual image wire block");
  if (object.type === "image_url") {
    const image = objectValue(object.image_url, "Actual image_url block");
    if (typeof image.url === "string" && image.url.startsWith("data:image/png;base64,")) return [pngPayload(image.url.slice("data:image/png;base64,".length))];
  }
  return Object.values(object).flatMap(imagePayloads);
}
