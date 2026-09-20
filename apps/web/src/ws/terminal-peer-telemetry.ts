// WebRTC peer telemetry retains only safe connection metadata for diagnostics.
// The peer adapter calls it after a selected ICE pair connects and authenticates.
// It intentionally records a candidate category, never SDP, candidates, addresses, or credentials.
import type { TerminalPeerCandidateType } from "../store/terminal-stream-transport.ts";

export class TerminalPeerTelemetry {
  private candidateTypeValue: TerminalPeerCandidateType = "none";
  private capturing = false;

  candidateType(peerConnection?: RTCPeerConnection): TerminalPeerCandidateType {
    if (this.candidateTypeValue === "none" && peerConnection) this.captureSelectedCandidateType(peerConnection);
    return this.candidateTypeValue;
  }
  captureSelectedCandidateType(peerConnection: RTCPeerConnection): void {
    if (typeof peerConnection.getStats !== "function") return;
    if (this.capturing) return;
    this.capturing = true;
    try {
      void peerConnection.getStats().then((report) => {
        this.candidateTypeValue = selectedRemoteCandidateType(report);
      }).catch(() => undefined).finally(() => { this.capturing = false; });
    } catch {
      this.capturing = false;
      // Diagnostics must not affect an authenticated terminal route.
    }
  }
}

function selectedRemoteCandidateType(report: RTCStatsReport): TerminalPeerCandidateType {
  let nominated: Record<string, unknown> | null = null;
  for (const entry of report.values()) {
    const pair = record(entry);
    if (!pair || pair.type !== "candidate-pair") continue;
    if (pair.selected === true) return candidateTypeForPair(report, pair);
    if (pair.nominated === true && pair.state === "succeeded") nominated = pair;
  }
  return nominated ? candidateTypeForPair(report, nominated) : "none";
}

function candidateTypeForPair(
  report: RTCStatsReport,
  pair: Record<string, unknown>,
): TerminalPeerCandidateType {
  const remoteCandidateId = pair.remoteCandidateId;
  if (typeof remoteCandidateId !== "string") return "none";
  const candidate = record(report.get(remoteCandidateId));
  if (!candidate) return "none";
  const candidateType = candidate.candidateType;
  return candidateType === "host" || candidateType === "srflx" || candidateType === "prflx"
    ? candidateType
    : "none";
}
function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" ? value as Record<string, unknown> : null;
}
