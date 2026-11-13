// Owns the Linux-only live signaling/sideband fixture and its real answering peer.
// The builtin registry supplies fresh owned seed/audio paths and captures ServiceTrace.
// Only physical network boundaries replay here; canonical controllers and parsers remain intact.

import { appendFileSync } from "node:fs";
import { lstat, realpath, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { ServerWebSocket } from "bun";
import type * as LiveProtocol from "@omp-source/packages/coding-agent/src/live/protocol";
import type { BuiltinSeedContext } from "./builtin-seed";
import { listeningPort, type LocalHttpService, type ServiceTrace } from "./fixture-service-http";
import { startLiveFixtureFeeder, type LiveFixtureFeeder } from "./live-peer-feeder";
import type { LivePeerMetrics } from "./live-peer-runtime";
import { startLiveFixturePeer, type LiveFixturePeer } from "./live-peer-fixture";

export interface LiveFixtureInput {
  mode: "conversation" | "failure";
  userTranscript: string;
  assistantTranscript: string;
  failureMessage: string;
}

interface LiveSnapshot {
  userTranscript: string;
  assistantTranscript: string;
  receivedAudio: boolean;
  ended: boolean;
  failed: boolean;
}
interface SocketData { callId: string }

export async function startLiveFixtureService(
  input: LiveFixtureInput, context: BuiltinSeedContext, trace: ServiceTrace,
): Promise<{ service: LocalHttpService; environment: Record<string, string> }> {
  if (process.platform !== "linux") throw new Error("Positive live parity requires Linux controlled ALSA; native Apple attestation is not isolated");
  if (input.mode !== "conversation" && input.mode !== "failure") throw new Error("Unsupported live fixture mode");
  for (const text of [input.userTranscript, input.assistantTranscript, input.failureMessage]) {
    if (typeof text !== "string" || !text) throw new Error("Live fixture protocol text must be nonempty");
  }
  const home = await realpath(context.home);
  const directory = join(home, ".parity");
  if ((await lstat(directory)).isSymbolicLink()) throw new Error("Live fixture directory must be owned, not a symlink");
  const wav = join(directory, "live-capture.wav");
  const capture = await lstat(wav);
  if (!capture.isFile() || capture.isSymbolicLink() || capture.size <= 44) throw new Error("Controlled live capture WAV must be prepared before service startup");
  const protocol = await context.load<typeof LiveProtocol>("packages/coding-agent/src/live/protocol.ts");
  const tracePath = join(directory, "live.jsonl");
  await writeFile(tracePath, "", { flag: "wx", mode: 0o600 });
  const state: LiveSnapshot = { userTranscript: "", assistantTranscript: "", receivedAudio: false, ended: false, failed: false };
  const snapshot = () => appendFileSync(tracePath, `${JSON.stringify({ channel: "builtin.live", value: { ...state } })}\n`);
  const record = (value: Record<string, unknown>) => trace("live", value);
  const callId = `rtc_${crypto.randomUUID()}`;
  let peer: LiveFixturePeer | undefined;
  let feeder: LiveFixtureFeeder | undefined;
  let socket: ServerWebSocket<SocketData> | undefined;
  let claimed = false;
  let upgraded = false;
  let peerClosed = false;
  let socketClosed = false;
  let connected = false;
  let inboundAudioPackets = 0;
  let inboundAudioBytes = 0;
  let outboundAudioPackets = 0;
  let dataChannelOpen = false;
  let emitted = false;
  let closing: Promise<void> | undefined;
  let disposal: Promise<void> | undefined;
  let deadline: NodeJS.Timeout | undefined;
  const socketEnded = Promise.withResolvers<void>();
  const failures: unknown[] = [];
  const tasks = new Set<Promise<void>>();
  const track = (operation: Promise<void>) => {
    tasks.add(operation);
    void operation.catch(error => { failures.push(error); }).finally(() => tasks.delete(operation));
  };
  const send = (payload: Record<string, unknown>) => {
    const raw = JSON.stringify(payload);
    const parsed = protocol.parseLiveServerEvent(raw);
    if (!parsed || parsed.type === "unknown") throw new Error("Fixture attempted a noncanonical live protocol event");
    if (!socket || socket.readyState !== 1 || socket.send(raw) === 0) throw new Error("Live fixture sideband is not writable");
    record({ transport: "websocket", direction: "send", raw, payload });
    switch (parsed.type) {
      case "input_transcript.added": state.userTranscript += parsed.item.text; break;
      case "output_transcript.added": state.assistantTranscript += parsed.item.text; break;
      case "turn.done":
        if (parsed.turn.role === "user") state.userTranscript = parsed.turn.transcript;
        else state.assistantTranscript = parsed.turn.transcript;
        break;
      case "error": state.failed = true; break;
    }
    snapshot();
  };
  const cleanupCall = (): Promise<void> => {
    closing ??= (async () => {
      clearTimeout(deadline);
      let closeFailure: unknown;
      let closed: LivePeerMetrics | undefined;
      try { closed = await peer?.close(); } catch (error) { closeFailure = error; }
      peerClosed = closed?.connectionState === "closed" && closed.streamsStopped;
      if (socket && !socketClosed) {
        socket.close(1000, "done");
        const timeout = setTimeout(() => { socket?.terminate(); }, 1_000);
        const expiry = setTimeout(() => socketEnded.reject(new Error("Live fixture sideband did not close")), 3_000);
        try { await socketEnded.promise; }
        catch (error) { closeFailure ??= error; }
        finally { clearTimeout(timeout); clearTimeout(expiry); }
      }
      if (claimed && upgraded && peerClosed && socketClosed) {
        state.ended = true;
        record({ transport: "state", ended: true, peerClosed, socketClosed });
        snapshot();
      }
      try { await feeder?.close(); } catch (error) { closeFailure ??= error; }
      try { await peer?.dispose(); } catch (error) { closeFailure ??= error; }
      try { await server.stop(true); } catch (error) { closeFailure ??= error; }
      if (closeFailure) throw closeFailure;
    })();
    return closing;
  };
  const emitReady = () => {
    if (closing || emitted || !socket || socket.readyState !== 1) return;
    if (!connected || !dataChannelOpen || inboundAudioPackets <= 0 || inboundAudioBytes <= 0 || outboundAudioPackets <= 0) return;
    emitted = true;
    send({ type: "session.started", session: { id: callId } });
    if (input.mode === "failure") send({ type: "error", message: input.failureMessage });
    else {
      send({ type: "input_transcript.added", item: { text: input.userTranscript } });
      send({ type: "turn.done", turn: { role: "user", transcript: input.userTranscript } });
      send({ type: "output_transcript.added", item: { text: input.assistantTranscript } });
      send({ type: "turn.done", turn: { role: "assistant", transcript: input.assistantTranscript } });
    }
  };
  const rejectProtocol = (message: string) => {
    try { send({ type: "error", message }); } catch (error) { failures.push(error); }
    track(cleanupCall());
  };
  const receive = (raw: string, transport: "websocket" | "datachannel") => {
    if (Buffer.byteLength(raw) > 64 * 1024) { rejectProtocol("Live fixture control frame too large"); return; }
    let payload: Record<string, unknown>;
    try {
      const parsed: unknown = JSON.parse(raw);
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("Expected live JSON object");
      payload = parsed as Record<string, unknown>;
    } catch { rejectProtocol("Malformed live fixture control JSON"); return; }
    record({ transport, direction: "receive", raw, payload });
    snapshot();
    if (payload.type === "session.close" && Object.keys(payload).length === 1) { track(cleanupCall()); return; }
    if ((payload.type === "session.context.append" || payload.type === "delegation.context.append")
      && (payload.channel === undefined || payload.channel === "speakable" || payload.channel === "commentary")
      && (payload.type !== "delegation.context.append" || typeof payload.delegation_item_id === "string")
      && Array.isArray(payload.content) && payload.content.every(item => item && typeof item === "object" && item.type === "input_text" && typeof item.text === "string")) return;
    rejectProtocol("Unexpected live fixture control type");
  };
  const observePeer = (value: Record<string, unknown>) => {
    record({ transport: "rtc", ...value });
    if (value.kind === "data") {
      if (typeof value.raw === "string") receive(value.raw, "datachannel");
      else rejectProtocol("Unexpected live fixture binary data frame");
      return;
    }
    if (value.kind === "failure" || value.connectionState === "failed" || value.iceConnectionState === "failed") {
      state.failed = true;
      snapshot();
      failures.push(new Error(typeof value.message === "string" ? value.message : "Live fixture peer failed"));
      if (peer) track(cleanupCall());
      return;
    }
    if (value.kind === "stats") {
      connected = value.connectionState === "connected";
      inboundAudioPackets = typeof value.inboundAudioPackets === "number" ? value.inboundAudioPackets : 0;
      inboundAudioBytes = typeof value.inboundAudioBytes === "number" ? value.inboundAudioBytes : 0;
      outboundAudioPackets = typeof value.outboundAudioPackets === "number" ? value.outboundAudioPackets : 0;
      dataChannelOpen = value.dataChannelState === "open";
      if (connected && inboundAudioPackets > 0 && inboundAudioBytes > 0) state.receivedAudio = true;
      snapshot();
      try { emitReady(); } catch (error) { failures.push(error); track(cleanupCall()); }
    }
  };
  const server = Bun.serve<SocketData>({
    hostname: "127.0.0.1", port: 0, maxRequestBodySize: 256 * 1024,
    async fetch(request, server) {
      const url = new URL(request.url);
      const origin = `http://127.0.0.1:${server.port}`;
      if (url.origin !== origin || request.headers.get("host") !== `127.0.0.1:${server.port}`
        || (request.headers.has("origin") && request.headers.get("origin") !== origin)) return new Response("Unexpected live fixture origin", { status: 403 });
      if (request.method === "GET" && url.pathname === "/peer" && !url.search) {
        return new Response("<!doctype html><html><head><link rel=\"icon\" href=\"data:,\"></head><body></body></html>", {
          headers: { "Content-Type": "text/html", "Content-Security-Policy": "default-src 'none'; media-src blob:; connect-src 'none'; frame-src 'none'", "Cache-Control": "no-store" },
        });
      }
      if (request.method === "GET" && url.pathname === `/v1/live/${callId}` && !url.search) {
        if (!claimed || upgraded || closing) return new Response("No available live fixture call", { status: 409 });
        upgraded = server.upgrade(request, { data: { callId } });
        if (upgraded) return;
        return new Response("Live WebSocket required", { status: 426 });
      }
      if (request.method !== "POST" || url.pathname !== "/backend-api/codex/realtime/calls" || url.search !== "?intent=quicksilver&architecture=avas") return new Response("Unknown live fixture route", { status: 404 });
      if (!peer || claimed || closing) return new Response("Live fixture permits one call", { status: 409 });
      if (request.headers.get("content-type")?.split(";", 1)[0] !== "application/json") return new Response("Live signaling requires JSON", { status: 415 });
      claimed = true;
      try {
        const raw = await request.text();
        const payload: unknown = JSON.parse(raw);
        if (!payload || typeof payload !== "object" || Array.isArray(payload)) throw new Error("Invalid signaling object");
        const body = payload as { sdp?: unknown; session?: unknown };
        if (typeof body.sdp !== "string" || !body.session || typeof body.session !== "object") throw new Error("Invalid real live offer/session");
        const session = body.session as { model?: unknown; instructions?: unknown; delegation?: { type?: unknown }; audio?: { output?: { voice?: unknown } } };
        if (session.model !== protocol.LIVE_MODEL || typeof session.instructions !== "string" || session.delegation?.type !== "client" || typeof session.audio?.output?.voice !== "string") throw new Error("Unexpected live session payload");
        record({ transport: "http", direction: "request", method: request.method, url: request.url, raw, payload });
        const answer = await peer.answer(body.sdp);
        if (closing) throw new Error("Live fixture call closed during signaling");
        const location = `https://api.openai.com/v1/live/${callId}`;
        record({ transport: "http", direction: "response", status: 201, contentType: "application/sdp", location, raw: answer });
        return new Response(answer, { status: 201, headers: { "Content-Type": "application/sdp", Location: location, "Cache-Control": "no-store" } });
      } catch (error) {
        failures.push(error);
        track(cleanupCall());
        return new Response("Live fixture signaling failed", { status: 400 });
      }
    },
    websocket: {
      maxPayloadLength: 64 * 1024, idleTimeout: 45,
      open(opened) {
        socket = opened;
        record({ transport: "websocket", direction: "open", callId: opened.data.callId });
        snapshot();
        try { emitReady(); } catch (error) { failures.push(error); track(cleanupCall()); }
      },
      message(_socket, message) {
        if (typeof message !== "string") { rejectProtocol("Unexpected live fixture binary sideband frame"); return; }
        receive(message, "websocket");
      },
      close(_socket, code, reason) {
        socketClosed = true;
        record({ transport: "websocket", direction: "close", code, reason });
        socketEnded.resolve();
        track(cleanupCall());
      },
    },
  });
  const dispose = (): Promise<void> => {
    disposal ??= (async () => {
      clearTimeout(deadline);
      try { await cleanupCall(); } catch (error) { failures.push(error); }
      try { await peer?.dispose(); } catch (error) { failures.push(error); }
      try { await feeder?.close(); } catch (error) { failures.push(error); }
      try { await server.stop(true); } catch (error) { failures.push(error); }
      await Promise.allSettled([...tasks]);
      if (failures.length) throw new AggregateError(failures, "Live fixture failed or did not cleanly dispose");
    })();
    return disposal;
  };
  try {
    const url = `http://127.0.0.1:${server.port}`;
    feeder = await startLiveFixtureFeeder(home, wav, value => {
      record({ transport: "process", ...value });
      if (value.kind === "feeder-failure" || (value.kind === "feeder-exit" && value.exitCode !== 0 && !value.stopping)) {
        failures.push(new Error(typeof value.message === "string" ? value.message : `Live audio feeder exited ${value.exitCode}`));
        if (peer) track(cleanupCall());
      }
    });
    peer = await startLiveFixturePeer(url, wav, observePeer);
    if (failures.length) throw new AggregateError(failures, "Live fixture browser initialization failed");
    deadline = setTimeout(() => {
      failures.push(new Error("Live fixture call exceeded its 45-second lifetime"));
      void dispose().catch(() => {});
    }, 45_000);
    return {
      service: { url, port: listeningPort(server), dispose },
      environment: { OMP_PARITY_LIVE_URL: url, OMP_PARITY_LIVE_WAV: wav, OMP_PARITY_LIVE_TRACE: tracePath, OMP_PARITY_LIVE_PCM_FIFO: feeder.fifo },
    };
  } catch (error) {
    try { await dispose(); }
    catch (cleanupError) { throw new AggregateError([error, cleanupError], "Live fixture startup and cleanup failed"); }
    throw error;
  }
}
