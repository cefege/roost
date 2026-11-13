// Owns the isolated fixture page's real microphone, RTCPeerConnection and stats.
// Called only by live-peer-fixture through Playwright evaluation on owned loopback.
// No product transport, parser, browser API or media-device implementation is replaced.

export interface LivePeerMetrics {
  connectionState: RTCPeerConnectionState;
  iceConnectionState: RTCIceConnectionState;
  inboundAudioPackets: number;
  inboundAudioBytes: number;
  inboundAudioSamples: number;
  outboundAudioPackets: number;
  dataChannelState: RTCDataChannelState | "absent";
  streamsStopped: boolean;
}

export interface LivePeerRuntime {
  answer(offer: string): Promise<string>;
  close(): Promise<LivePeerMetrics>;
}

export type LivePeerWindow = Window & typeof globalThis & {
  __ompParityLivePeer: LivePeerRuntime;
  __ompParityLivePeerEvent(value: Record<string, unknown>): Promise<void>;
};

export function installLivePeerRuntime(): void {
  const fixture = window as LivePeerWindow;
  let peer: RTCPeerConnection | undefined;
  let capture: MediaStream | undefined;
  const inboundTracks = new Set<MediaStreamTrack>();
  let channel: RTCDataChannel | undefined;
  let playback: HTMLAudioElement | undefined;
  let polling: number | undefined;
  let sampling = false;
  let closing: Promise<LivePeerMetrics> | undefined;
  let streamsStopped = false;
  let lastMetrics: LivePeerMetrics | undefined;
  let gatheredCancel: (() => void) | undefined;
  let pendingEvents: Promise<void> = Promise.resolve();

  const report = (value: Record<string, unknown>): void => {
    pendingEvents = pendingEvents.then(() => fixture.__ompParityLivePeerEvent(value));
    void pendingEvents.catch(() => {});
  };
  const metrics = async (): Promise<LivePeerMetrics> => {
    let inboundAudioPackets = 0;
    let inboundAudioBytes = 0;
    let inboundAudioSamples = 0;
    let outboundAudioPackets = 0;
    if (peer && peer.connectionState !== "closed") {
      const statistics = await peer.getStats();
      statistics.forEach(statistic => {
        if (statistic.kind !== "audio" && statistic.mediaType !== "audio") return;
        if (statistic.type === "inbound-rtp") {
          inboundAudioPackets += statistic.packetsReceived ?? 0;
          inboundAudioBytes += statistic.bytesReceived ?? 0;
          inboundAudioSamples += statistic.totalSamplesReceived ?? 0;
        } else if (statistic.type === "outbound-rtp") {
          outboundAudioPackets += statistic.packetsSent ?? 0;
        }
      });
    } else if (lastMetrics) {
      ({ inboundAudioPackets, inboundAudioBytes, inboundAudioSamples, outboundAudioPackets } = lastMetrics);
    }
    return {
      connectionState: peer?.connectionState ?? "new",
      iceConnectionState: peer?.iceConnectionState ?? "new",
      inboundAudioPackets, inboundAudioBytes, inboundAudioSamples, outboundAudioPackets,
      dataChannelState: channel?.readyState ?? "absent", streamsStopped,
    };
  };
  const sample = async (): Promise<void> => {
    if (sampling || closing) return;
    sampling = true;
    try {
      lastMetrics = await metrics();
      report({ kind: "stats", ...lastMetrics });
    } catch (error) {
      if (!closing) report({ kind: "failure", message: String(error) });
    } finally { sampling = false; }
  };
  const close = (): Promise<LivePeerMetrics> => {
    if (closing) return closing;
    closing = (async () => {
      clearInterval(polling);
      polling = undefined;
      gatheredCancel?.();
      if (channel) {
        channel.onopen = null;
        channel.onclose = null;
        channel.onmessage = null;
        channel.close();
      }
      capture?.getTracks().forEach(track => track.stop());
      inboundTracks.forEach(track => track.stop());
      streamsStopped = (!capture || capture.getTracks().every(track => track.readyState === "ended"))
        && [...inboundTracks].every(track => track.readyState === "ended");
      if (playback) { playback.pause(); playback.srcObject = null; playback.remove(); }
      if (peer) {
        peer.onconnectionstatechange = null;
        peer.oniceconnectionstatechange = null;
        peer.ondatachannel = null;
        peer.ontrack = null;
        peer.close();
      }
      lastMetrics = await metrics();
      report({ kind: "closed", ...lastMetrics });
      await pendingEvents;
      return lastMetrics;
    })();
    return closing;
  };

  fixture.__ompParityLivePeer = {
    close,
    async answer(offer) {
      if (peer || closing) throw new Error("Live fixture permits exactly one media peer");
      try {
        peer = new RTCPeerConnection({ iceServers: [], iceTransportPolicy: "all" });
        peer.onconnectionstatechange = () => { void sample(); };
        peer.oniceconnectionstatechange = () => { void sample(); };
        peer.ondatachannel = event => {
          if (channel || event.channel.label !== "oai-events") {
            event.channel.close();
            report({ kind: "failure", message: "Unexpected live data channel" });
            return;
          }
          channel = event.channel;
          channel.onopen = () => { void sample(); };
          channel.onclose = () => { void sample(); };
          channel.onmessage = message => report({ kind: "data", raw: typeof message.data === "string" ? message.data : null });
        };
        peer.ontrack = event => {
          if (event.track.kind !== "audio" || inboundTracks.size > 0) {
            event.track.stop();
            report({ kind: "failure", message: "Unexpected live media track" });
            return;
          }
          inboundTracks.add(event.track);
          playback = document.createElement("audio");
          playback.autoplay = true;
          playback.muted = true;
          playback.srcObject = new MediaStream([event.track]);
          document.body.append(playback);
          void playback.play().catch(error => report({ kind: "failure", message: String(error) }));
          report({ kind: "track", mediaKind: event.track.kind, readyState: event.track.readyState });
        };
        capture = await navigator.mediaDevices.getUserMedia({ audio: true, video: false });
        if (closing) {
          capture.getTracks().forEach(track => track.stop());
          throw new Error("Live fixture closed during microphone acquisition");
        }
        for (const track of capture.getAudioTracks()) peer.addTrack(track, capture);
        await peer.setRemoteDescription({ type: "offer", sdp: offer });
        await peer.setLocalDescription(await peer.createAnswer());
        const gathered = Promise.withResolvers<void>();
        const cleanup = () => { clearTimeout(timeout); peer?.removeEventListener("icegatheringstatechange", changed); gatheredCancel = undefined; };
        const changed = () => { if (peer?.iceGatheringState === "complete") { cleanup(); gathered.resolve(); } };
        const timeout = setTimeout(() => { cleanup(); gathered.reject(new Error("Live fixture ICE gathering timed out")); }, 8_000);
        gatheredCancel = () => { cleanup(); gathered.reject(new Error("Live fixture closed during ICE gathering")); };
        peer.addEventListener("icegatheringstatechange", changed);
        changed();
        await gathered.promise;
        const answer = peer.localDescription?.sdp;
        if (!answer) throw new Error("Real live fixture peer produced no SDP answer");
        polling = window.setInterval(() => { void sample(); }, 100);
        await sample();
        return answer;
      } catch (error) {
        await close();
        throw error;
      }
    },
  };
}
