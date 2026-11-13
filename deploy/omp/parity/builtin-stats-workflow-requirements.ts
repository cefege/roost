import type { BuiltinRequirements } from "./builtin-requirements";

export interface StatsWorkflowProfile {
  isolation: "fresh-process-owned-home-and-exclusive-loopback-ports";
  ports: { dashboard: 3847; ignoredRequest: 3848; hostname: "127.0.0.1" };
  hosts: "local-terminal-hosted-browser" | "view-owned-remote-host-services-both-legs";
  statsReplies: Array<"accept" | "host-reject" | "operator-dismiss">;
  traceReplies: "accept-after-owned-http-identity";
  http: {
    origin: "http://127.0.0.1:3847"; subscribe: "/api/events"; session: "original-manifest-session";
    browserTransport: "real-owned-roost-capability";
    terminalTransport: "native-worker-origin" | "real-owned-roost-capability";
  };
  cleanup: "real-restart-owner-then-final-process-owner-teardown";
}

export const statsWorkflowCaptureSource = "Observe actual owned stats server HTTP responses, SSE ingest completion, OS listener/process ownership and real host-service/opener requests according to StatsWorkflowProfile. Implement the exact pointer contracts exported by builtin-stats-workflow-requirements.ts; never return profile inputs or expected values as observations.";

export const statsWorkflowCapturePointers: Readonly<Record<string, string>> = {
  "/http/dashboardStatus": "GET / from the actual stats transport selected by the profile (native worker origin for a local terminal; actual minted /omp-stats/<64hex>/ frontdoor capability for a hosted leg); return response.status. Require the actual SDK HTML and referenced production assets, never synthetic responses or HTML rewriting.",
  "/http/modelsStatus": "GET /api/stats/models through that actual transport; return response.status.",
  "/http/securityVersion": "At the worker boundary read x-omp-stats-dashboard from an actual SDK /api/stats/models response; preserve its string value. This private identity is intentionally not forwarded to the browser.",
  "/http/bindHostname": "At the worker boundary read x-omp-stats-hostname from an actual SDK /api/stats/models response.",
  "/http/contentPreserved": "Require actual GET / HTML, index.js and index.css bytes to equal the production assets served by the SDK. For a hosted transport compare actual frontdoor HTTP bodies with actual upstream worker responses, preserving bytes and relative asset paths; verify no worker identity/resource headers, Set-Cookie or Location escaped. A status-only probe is insufficient.",
  "/http/sseNativeBytes": "Read actual /api/events bytes through the selected transport. For a hosted leg correlate the real worker upstream response body chunks with the frontdoor stream and compare the same concatenated byte sequence, including SDK data events and heartbeats, across chunk boundaries. Retain raw evidence and require real initial sync status. Separate subscriptions, /api/status polling, reconstructed JSON/events or synthetic heartbeats do not prove relay.",
  "/server/registeredResourceOwned": "Verify the actual listening PID belongs to this run's canonical OMP child and its real SDK response has a 64-lowercase-hex x-omp-stats-resource. In every hosted leg also require the actual dashboardResource active notification (session/request/view/transport), real worker child-generation registration and open_stats_dashboard resourceId all match that actual header. An arbitrary local port plus spoofable dashboard version headers is not ownership.",
  "/http/syncPhase": "Maintain the selected transport's real GET /api/events subscription; await actual completed initial sync with non-null lastSyncedAt and no error, then return actual GET /api/status JSON sync.phase through that transport. Timeout or sync error fails.",
  "/http/syncError": "Return actual GET /api/status JSON sync.error through the selected transport after completed initial sync; do not clear errors.",
  "/http/sessionListStatus": "GET /api/sessions?limit=100 after ingest; return response.status.",
  "/http/sessionListMatchesOriginal": "Find the actual /api/sessions row whose resolved file equals the original manifest.sessionPath; return whether its file matches. Do not accept an arbitrary row, title-only match, or disk-scan fallback as imported evidence.",
  "/http/importedOriginalSession": "After SSE reports completed ingest, open getStatsDbPath() READ ONLY and query SELECT EXISTS(SELECT 1 FROM messages WHERE session_file = ?) using original manifest.sessionPath. Return actual row existence; HTTP /api/sessions alone also includes unindexed disk roots and cannot establish import. Never call a parser/sync method from this observer.",
  "/http/traceStatus": "GET /api/session/trace?file=encodeURIComponent(original manifest.sessionPath), return response.status.",
  "/http/traceFileMatchesOriginal": "Compare actual trace response.file and its tracks entry id=main.file, resolved, to original manifest.sessionPath; true only when BOTH match.",
  "/http/traceTitle": "Return actual /api/session/trace response.title, not the supplied seed title.",
  "/http/seedUserText": "Record actual seeded user entry id from the real persisted session parser. GET /api/session/entry?file=<encoded original manifest.sessionPath>&id=<encoded actual seed user id>; require HTTP 200 and return response.entry.message.content joined text blocks (string content unchanged). Never read expected seed text from the manifest for this value.",
  "/http/seedAssistantText": "As for seedUserText, fetch the actual seeded assistant entry id through /api/session/entry and return actual response.entry.message.content text.",
  "/http/traceRevalidationStatus": "GET the same /api/session/trace twice without intervening session writes; send the first actual ETag as If-None-Match on the second request. Return the second status, retaining both raw responses. Never manufacture an ETag.",
  "/server/listenerCount": "Count actual owned OS listening sockets for this scenario's stats origins, deduplicated by address/port. Do not count HTTP keepalive sockets or infer count from submitted commands.",
  "/server/sameListenerAsFirst": "Compare current actual listening PID and origin to the first observed stats listener PID and origin in this process generation; both must match. Keep that baseline across repeat launches and rejected/dismissed opens.",
  "/host/statsRequestCount": "Count actual open_stats_dashboard requests with parsed value.fragment empty at the owned host-service channel; preserve original serviceId, resourceId and full request/view/transport envelope. Local terminal mode instead counts real native openPath opener argv targeting the bare dashboard origin. Exclude trace-fragment opens; never count notices or probes.",
  "/host/statsAcceptedCount": "Count actual accepted replies consumed by RemoteHostServices.reply for those empty-fragment stats requests, or completed owned native opener invocations in local terminal mode. Do not count merely scheduled replies.",
  "/host/statsRejectedCount": "Count actual empty-fragment stats replies consumed by production RemoteHostServices.reply with accepted=false and explicit source-backed error 'The browser did not accept this host service.'. The real Rust host reply path requires a nonempty error on rejection.",
  "/host/statsDismissedCount": "Count actual empty-fragment stats replies consumed by RemoteHostServices.reply with accepted=false and production dismissal error 'The operator dismissed this request.'. Do not infer from the plan.",
  "/host/pendingStatsRequests": "Count outstanding actual empty-fragment open_stats_dashboard service ids after production settlement; unmatched replies or a still-pending promise fail.",
  "/host/traceOpenMatchesOriginal": "Inspect the latest actual native opener argv (local terminal) OR dedicated open_stats_dashboard request JSON {resourceId,origin,fragment} (hosted). Require origin http://127.0.0.1:3847, fragment #/traces?s=<encoded path> with exactly one s resolving to original manifest.sessionPath. Hosted capture must additionally prove the actual minted frontdoor capability carries that same exact fragment and its actual proxied /api/session/trace response.file/main track match the original. Never substitute generic open_url, a blocked-loopback notice, session state or an invented capability.",
  "/host/traceOpenCount": "Count actual local opener argv or view-owned open_stats_dashboard requests with the saved-session trace fragment. Retain actual raw URL/JSON, service id and origin envelope; do not count status text.",
  "/host/traceBlockedNoticeCount": "Count actual warning notices emitted by the production UI with exact text 'This worker-local or unsupported URL cannot be opened in the browser.'. Zero is required for positive trace workflows; never suppress this notice or reinterpret it as an open.",
  "/cleanup/previousListenerClosed": "After /restart has actually exited the prior PID and the real process owner has restarted the same saved session, verify the old listening PID exited, its 3847 socket disappeared and a fresh connection to the former origin is refused. A replacement dashboard listener is NOT cleanup success.",
  "/cleanup/sseClosed": "After actual prior process exit, confirm the observer's real /api/events stream reached EOF/error and its reader was cancelled/released; do not report closed while a stream remains attached.",
  "/cleanup/capabilityUnavailable": "After real child/transport teardown, GET every actual hosted capability retained by this case and require it is no longer usable (not HTTP 200); verify the worker registry removed the old child-generation resource and every upstream stream lease. In a local-only terminal boundary require its recorded SDK resource/listener is gone, not an invented capability.",
};

const ids = ["stats-launch-reuse-trace", "trace-persisted-rerun-stats", "stats-host-rejection-recover", "stats-host-dismiss-recover"] as const;

export function applyStatsWorkflowRequirements(requirements: Record<string, BuiltinRequirements>): void {
  for (const id of ids) {
    const requirement = requirements[`builtin-${id}`];
    if (!requirement) throw new Error(`Missing stats workflow builtin-${id}`);
    requirement.session = {
      storage: "persisted", seed: "transcript", title: "Parity stats workflow",
      userText: "PARITY_STATS_USER", assistantText: "PARITY_STATS_ASSISTANT", additionalSavedSessions: [],
    };
    requirement.models.turns = [];
    requirement.lifecycle.superviseRealRestart = true;
    requirement.lifecycle.resumeSameSession = true;
    requirement.statsWorkflow = {
      isolation: "fresh-process-owned-home-and-exclusive-loopback-ports",
      ports: { dashboard: 3847, ignoredRequest: 3848, hostname: "127.0.0.1" },
      hosts: id.startsWith("stats-host-") ? "view-owned-remote-host-services-both-legs" : "local-terminal-hosted-browser",
      statsReplies: id === "stats-host-rejection-recover" ? ["host-reject", "accept", "accept"]
        : id === "stats-host-dismiss-recover" ? ["operator-dismiss", "accept", "accept"]
          : id === "trace-persisted-rerun-stats" ? ["accept", "accept"] : ["accept", "accept", "accept"],
      traceReplies: "accept-after-owned-http-identity",
      http: {
        origin: "http://127.0.0.1:3847", subscribe: "/api/events", session: "original-manifest-session",
        browserTransport: "real-owned-roost-capability",
        terminalTransport: id.startsWith("stats-host-") ? "real-owned-roost-capability" : "native-worker-origin",
      },
      cleanup: "real-restart-owner-then-final-process-owner-teardown",
    };
    for (const capture of requirement.captures) {
      if (capture.channel !== "builtin.stats") continue;
      const source = capture.pointer && statsWorkflowCapturePointers[capture.pointer];
      if (!source) throw new Error(`Undeclared stats capture ${capture.pointer ?? "<root>"}`);
      capture.source = source;
    }
    requirement.prerequisites.push(
      "Each leg runs a fresh canonical OMP child in its own owned HOME. Hold an exclusive ownership lock for loopback ports 3847 and 3848; prove both are unoccupied BEFORE canonical SDK startup. Refuse the case if another owner occupies either port: never call stats.prepareStatsPort against an operator listener or kill an unrecorded PID. Deny external provider/network requests and inherited operator credentials.",
      "Set HOME to the owned home BEFORE importing source; set PI_CONFIG_DIR=.omp and PI_CODING_AGENT_DIR=<owned HOME>/.omp/agent. Unset OMP_PROFILE, PI_PROFILE, XDG_CONFIG_HOME, XDG_DATA_HOME, XDG_STATE_HOME, XDG_CACHE_HOME, TERN_PANE_SOCKET and TERN_PANE. OMP_CONFIG_DIR is not the canonical directory override. Seed through actual SessionManager under actual getSessionsDir(); flush before opening stats. Keep manifest.sessionPath and seed entry ids immutable across restart.",
      "Use actual production SDK HTML/assets, sqlite, startServer, StatsLive and the canonical launchSessionStatsDashboard/launchStatsDashboard/StatsNotice path. Only actual resource registration and host delivery plumbing differ for hosted transport. Never replace server/controller/judge/Tern implementations or prestart the dashboard; the submitted slash command must start it.",
      "For local-terminal-hosted-browser, preserve the canonical local-terminal host mode. Tern is naturally absent after environment scrub, so real openTernPage returns null and real openPath spawns the platform opener. Install an owned opener executable on PATH (xdg-open/open as appropriate) that records actual argv and exits 0 without external navigation; capture only that OS boundary. Hosted /stats uses the actual view-owned openStatsDashboard capability and production RemoteHostServices origin routing.",
      "For view-owned-remote-host-services-both-legs, drive the real TUI renderer and real native DOM editor through canonical RemoteCommandUi and RemoteHostServices in BOTH legs. A bare terminal openPath cannot model a host refusal because it never throws. Do not patch a local ctx.hostServices field or fabricate a remote view. If the runtime cannot host this mode it is a missing implementation, not a skipped case.",
      "Consume statsReplies ONLY for actual empty-fragment stats requests; trace requests independently use traceReplies. accept only after actual owned HTTP readiness. host-reject sends accepted=false/error='The browser did not accept this host service.' at the real host reply boundary; operator-dismiss uses the actual host dismissal accepted=false/error='The operator dismissed this request.'. Preserve original serviceId and origin; await real settlement. No direct handlers or injected notices.",
      "Every hosted leg uses the actual Roost worker/coordinator authenticated native view and existing /omp-stats/<opaque token>/ bridge in owned local processes, with a fresh owned account/worker key and an actual OS-assigned frontdoor port (never production). Worker starts the patched source OMP through its actual ProcessLauncher/ACP path and loads the original saved session; retain actual conversation/worker/process/transport identity. Capture actual SDK handle registration via channel.dashboardResource and _omp/ui/dashboard_resource; production worker registry binds it to its real ACP child generation, not a client PID or untrusted URL. Parent owns this process/auth/HTTP capture hook; an unimplemented hook is a failed acceptance criterion, never a gate or fabricated success.",
      "Canonical hosted /trace must call the dedicated actual owned-server stats adapter with the exact saved-session trace fragment. Generic hostedOpenTarget remains strict and must still reject arbitrary loopback URLs. Require actual accepted owned trace open plus actual proxied trace HTTP identity. Browser resolves only the real minted capability; no broad localhost allowlist, arbitrary port proxy, fake frontend capability or HTML rewrite.",
      "Subscribe through the selected transport's real /api/events; retain raw upstream and downstream bytes so hosted cases prove actual SDK SSE rather than status snapshots. Await idle sync with lastSyncedAt non-null/error=null, then snapshot actual /, assets, /api/stats/models, /api/status, /api/sessions, /api/session/trace and seeded /api/session/entry. Confirm messages.session_file import read-only separately. Do not invoke judgment/provider-window/auth routes.",
      "Use the real /restart owner: await old child exit, close/release SSE readers and owned opener children, restart the same original saved session, then capture prior listener refusal and actual seed screen/native DOM. SDK onStop and actual ACP child/transport exit revoke resources; retained hosted capabilities must be unusable and worker stream leases closed. No fake shutdown callback/direct singleton clearing. At final checkpoint and on failure terminate/reap every recorded OMP/worker/coordinator/opener child, close observers, release owned ports and lock, then delete owned HOME. Never terminate an unrecorded process.",
    );
  }
}
