import type { CoordinatorMoveService } from "./orchestrator.ts";

function handoffCredentials(request: Request): { handoffId: string; secret: string } | null {
  const handoffId = request.headers.get("x-roost-handoff-id");
  const secret = request.headers.get("x-roost-handoff-secret");
  return handoffId && secret ? { handoffId, secret } : null;
}

export async function handleInternalHandoffRequest(request: Request, move: CoordinatorMoveService): Promise<Response | null> {
  const path = new URL(request.url).pathname;
  if (!path.startsWith("/internal/coord-handoff/")) return null;
  const credentials = handoffCredentials(request);
  if (!credentials) return new Response("unauthorized", { status: 401 });
  try {
    if (path === "/internal/coord-handoff/status" && request.method === "GET") {
      const state = await move.internalStatus(credentials.handoffId, credentials.secret);
      return Response.json({
        phase: state.phase,
        source_url: state.source_url,
        target_url: state.target_url,
        expected_worker_fps: state.expected_worker_fps,
        connected_worker_fps: state.connected_worker_fps,
        error: state.error ?? null,
      });
    }
    if (path === "/internal/coord-handoff/commit" && request.method === "POST") {
      await move.internalCommit(credentials.handoffId, credentials.secret);
      return new Response(null, { status: 202 });
    }
    if (path === "/internal/coord-handoff/abort" && request.method === "POST") {
      await move.internalAbort(credentials.handoffId, credentials.secret);
      return new Response(null, { status: 204 });
    }
    return new Response("not found", { status: 404 });
  } catch (error) {
    // internalStatus rejects an unknown id or a bad secret with the same
    // message; that is an auth failure, not a precondition failure. The caller
    // (abortTarget) branches on 4xx-vs-transport, so the distinction matters.
    const message = (error as Error).message;
    return new Response(message, { status: message === "handoff not found" ? 401 : 412 });
  }
}
