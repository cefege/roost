import { appendFile, mkdir } from "node:fs/promises";
import { dirname } from "node:path";
import type { BuiltinProviderTurn } from "./builtin-requirements";
import { objectValue, stringValue } from "./runtime";
import { isDeniedModelDiscovery, isEscapingRequest } from "./network-classification";

export type ProviderTurn = BuiltinProviderTurn;

export interface ProviderSession {
  baseUrl: string;
  release(turnIndex: number): void;
  assertConsumed(): Promise<void>;
  stop(): Promise<void>;
}

export interface ProviderTraceOptions {
  networkTracePath?: string;
  serviceTracePath?: string;
}

function messageText(message: Record<string, unknown>): string {
  if (typeof message.content === "string") return message.content;
  if (!Array.isArray(message.content)) return "";
  return message.content.map(part => {
    const value = objectValue(part, "provider content part");
    return typeof value.text === "string" ? value.text : "";
  }).join("\n");
}

async function traceRows(path: string | undefined): Promise<Record<string, unknown>[]> {
  if (!path || !await Bun.file(path).exists()) return [];
  const text = await Bun.file(path).text();
  const complete = text.endsWith("\n") ? text : text.slice(0, text.lastIndexOf("\n") + 1);
  return complete.split("\n").filter(Boolean).map(line => objectValue(JSON.parse(line), "boundary trace"));
}

export async function startProvider(turns: readonly ProviderTurn[], tracePath: string, options: ProviderTraceOptions = {}): Promise<ProviderSession> {
  await mkdir(dirname(tracePath), { recursive: true });
  const consumed = new Set<number>();
  let cancelled = false;
  let disposed = false;
  let pending = Promise.resolve();
  const unexpected: string[] = [];
  const requests: Record<string, unknown>[] = [];
  const held = new Map<number, { release(): void; cancel(): void }>();
  const activeStreams = new Map<number, { cancel(): void; finished: Promise<void> }>();
  const record = (raw?: unknown) => {
    pending = pending.then(async () => {
      const network = await traceRows(options.networkTracePath);
      const services = await traceRows(options.serviceTracePath);
      const requestMessages = (request: Record<string, unknown>) => Array.isArray(request.messages)
        ? request.messages.map(message => objectValue(message, "provider message")) : [];
      const messages = requests.flatMap(requestMessages);
      const latestMessages = requests.length ? requestMessages(requests[requests.length - 1]!) : [];
      const text = messages.map(messageText).join("\n");
      const userPrompts = [...new Set(requests.flatMap(request => {
        const last = requestMessages(request).findLast(message => message.role === "user");
        return last ? [messageText(last)] : [];
      }))];
      const toolNames = [...new Set(messages.flatMap(message => Array.isArray(message.tool_calls)
        ? message.tool_calls.map(call => stringValue(objectValue(objectValue(call, "tool call").function, "tool function").name, "tool name")) : []))];
      const toolResults = messages.filter(message => message.role === "tool").map(message => ({ id: message.tool_call_id, content: messageText(message) }));
      const imageBlocks = latestMessages.flatMap(message => Array.isArray(message.content) ? message.content : [])
        .filter(part => part !== null && typeof part === "object" && "type" in part && ["image", "image_url", "input_image"].includes(String(part.type))).length;
      const guardInstalled = network.some(row => row.type === "guard-installed");
      const denied = services.filter(row => row.channel === "builtin.service" && typeof row.value === "object" && row.value !== null && objectValue(row.value, "service trace").service === "deny").length;
      const remoteRequests = network.filter(isEscapingRequest).length + denied;
      const deniedModelDiscovery = network.filter(isDeniedModelDiscovery).length;
      const value = {
        requests, requestCount: requests.length, userPrompts, toolNames, toolResults, imageBlocks, cancelled,
        ...(guardInstalled ? { remoteRequests, deniedModelDiscovery } : {}),
        guidedGoalPromptIncluded: text.includes("Parity objective") && /interview|clarif|question/i.test(text),
        compactionInstructionIncluded: text.includes("Parity compaction instruction"),
        backgroundWorkIncluded: text.includes("Parity background work"),
        seanceHistoryIncluded: text.includes("Parity historical question") && text.includes("Parity historical answer"),
      };
      await appendFile(tracePath, `${JSON.stringify({ channel: "builtin.provider", value, ...(raw === undefined ? {} : { raw }) })}\n`);
    });
    return pending;
  };
  const server = Bun.serve({
    hostname: "127.0.0.1", port: 0, idleTimeout: 0,
    async fetch(request) {
      const url = new URL(request.url);
      if (request.method !== "POST" || url.pathname !== "/v1/chat/completions") {
        const message = `Unexpected provider route: ${request.method} ${url.pathname}`;
        unexpected.push(message);
        await record({ error: message });
        return Response.json({ error: { message } }, { status: 409 });
      }
      let body: Record<string, unknown> | undefined;
      let model: string;
      let turnIndex: number;
      let turn: ProviderTurn;
      try {
        body = objectValue(await request.json(), "provider request");
        model = stringValue(body.model, "provider model");
        // Purpose labels are fixture annotations, not wire metadata for routing concurrent requests.
        turnIndex = turns.findIndex((candidate, index) => !consumed.has(index)
          && (candidate.match.modelId === undefined || candidate.match.modelId === model));
        if (turnIndex < 0) throw new Error(`Unscripted provider request for model ${JSON.stringify(model)}`);
        turn = turns[turnIndex]!;
        const messages = Array.isArray(body.messages) ? body.messages.map(message => objectValue(message, "provider message")) : [];
        if (turn.match.textIncludes && !messages.some(message => messageText(message).includes(turn.match.textIncludes ?? ""))) {
          throw new Error(`Provider turn ${turnIndex} for model ${JSON.stringify(model)} did not contain ${JSON.stringify(turn.match.textIncludes)}`);
        }
        if (turn.holdAfterText && !body.stream) throw new Error(`Provider turn ${turnIndex} requires the real streaming adapter`);
      } catch (cause) {
        const message = cause instanceof Error ? cause.message : String(cause);
        unexpected.push(message);
        await record({ error: message, ...(body === undefined ? {} : { request: body }) });
        return Response.json({ error: { message } }, { status: 409 });
      }
      consumed.add(turnIndex);
      requests.push(body);
      await record({ index: turnIndex, request: body });
      if (turn.failure) return Response.json({ error: { message: turn.failure.message, type: "fixture_error" } }, { status: turn.failure.status });
      const toolCalls = turn.toolCalls?.map(tool => ({ id: tool.id, type: "function", function: { name: tool.name, arguments: tool.argumentChunks?.join("") ?? JSON.stringify(tool.arguments) } }));
      const finish = toolCalls?.length ? "tool_calls" : "stop";
      const usage = { prompt_tokens: 12, completion_tokens: 8, total_tokens: 20 };
      const text = turn.textChunks?.join("") ?? turn.text ?? null;
      if (!body.stream) {
        return Response.json({ id: `fixture-${turnIndex}`, object: "chat.completion", created: 1_700_000_000, model, choices: [{ index: 0, message: { role: "assistant", content: text, ...(turn.thinking ? { reasoning_content: turn.thinking } : {}), tool_calls: toolCalls }, finish_reason: finish }], usage });
      }
      const chunks: Record<string, unknown>[] = [];
      const chunk = (delta: Record<string, unknown>, reason: string | null = null, final = false) => {
        const value = { id: `fixture-${turnIndex}`, object: "chat.completion.chunk", created: 1_700_000_000, model, choices: [{ index: 0, delta, finish_reason: reason }], ...(final ? { usage } : {}) };
        chunks.push(value);
        return value;
      };
      chunk({ role: "assistant" });
      if (turn.thinking) for (const character of turn.thinking) chunk({ reasoning_content: character });
      for (const content of turn.textChunks ?? (turn.text ? [...turn.text] : [])) chunk({ content });
      for (const [index, call] of (toolCalls ?? []).entries()) {
        chunk({ tool_calls: [{ index, id: call.id, type: "function", function: { name: call.function.name, arguments: "" } }] });
        for (const fragment of turn.toolCalls?.[index]?.argumentChunks ?? [...call.function.arguments]) chunk({ tool_calls: [{ index, function: { arguments: fragment } }] });
      }
      const encoder = new TextEncoder();
      const gate = Promise.withResolvers<"complete" | "cancel">();
      let streamCancelled = false;
      let completed = false;
      const finished = Promise.withResolvers<void>();
      const cancel = () => {
        if (completed || streamCancelled) return;
        streamCancelled = true;
        cancelled = true;
        held.delete(turnIndex);
        gate.resolve("cancel");
        void record({ index: turnIndex, cancelled: true });
      };
      if (turn.holdAfterText) held.set(turnIndex, {
        release() { held.delete(turnIndex); gate.resolve("complete"); }, cancel,
      });
      activeStreams.set(turnIndex, { cancel, finished: finished.promise });
      const stream = new ReadableStream<Uint8Array>({
        async start(controller) {
          request.signal.addEventListener("abort", cancel, { once: true });
          try {
            if (request.signal.aborted) { cancel(); return; }
            for (const entry of chunks) {
              if (streamCancelled || request.signal.aborted) return;
              controller.enqueue(encoder.encode(`data: ${JSON.stringify(entry)}\n\n`));
              if ((turn.chunkDelayMs ?? 1) > 0) await Bun.sleep(turn.chunkDelayMs ?? 1);
            }
            if (turn.holdAfterText && await gate.promise === "cancel") return;
            if (streamCancelled || request.signal.aborted) return;
            controller.enqueue(encoder.encode(`data: ${JSON.stringify(chunk({}, finish, true))}\n\n`));
            controller.enqueue(encoder.encode("data: [DONE]\n\n"));
            completed = true;
            await record({ index: turnIndex, response: chunks });
          } catch (error) {
            if (!streamCancelled && !request.signal.aborted) {
              const message = `Provider stream ${turnIndex}: ${error instanceof Error ? error.message : String(error)}`;
              unexpected.push(message);
              await record({ error: message });
              controller.error(error);
            }
          } finally {
            request.signal.removeEventListener("abort", cancel);
            held.delete(turnIndex);
            activeStreams.delete(turnIndex);
            finished.resolve();
            try { controller.close(); } catch { /* The actual HTTP client may have already closed the stream. */ }
          }
        },
        cancel,
      });
      return new Response(stream, { headers: { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" } });
    },
  });
  const timer = setInterval(() => void record(), 100);
  return {
    baseUrl: `http://127.0.0.1:${server.port}/v1`,
    release(turnIndex) {
      const turn = held.get(turnIndex);
      if (!turn) throw new Error(`Provider turn ${turnIndex} is not an active held stream`);
      turn.release();
    },
    async assertConsumed() {
      await record();
      if (unexpected.length) throw new Error(unexpected.join("\n"));
      if (consumed.size !== turns.length) throw new Error(`Provider tape consumed ${consumed.size}/${turns.length} turns; missing indices: ${turns.map((_, index) => index).filter(index => !consumed.has(index)).join(", ")}`);
      if (held.size) throw new Error("Actual provider streams remain held without completion or cancellation");
      if (activeStreams.size) throw new Error(`Actual provider streams have not settled: ${[...activeStreams.keys()].join(", ")}`);
      const violations = (await traceRows(options.networkTracePath)).filter(isEscapingRequest);
      if (violations.length) throw new Error(`Unexpected outbound provider traffic: ${JSON.stringify(violations)}`);
    },
    async stop() {
      if (disposed) return;
      disposed = true;
      clearInterval(timer);
      const streams = [...activeStreams.values()];
      for (const stream of streams) stream.cancel();
      await server.stop(true);
      await Promise.all(streams.map(stream => stream.finished));
      await pending;
    },
  };
}
