// audioPcmCapture — getUserMedia → AudioWorklet → linear16 PCM @ 16 kHz mono.
// Emits Int16Array chunks (~40 ms) for streaming to Deepgram's WS, matching the
// wispr native capture (encoding=linear16, sample_rate=16000, channels=1).
// AudioWorklet (not MediaRecorder) because iOS Safari MediaRecorder is unreliable
// while getUserMedia + AudioWorklet works from iOS 14.5+.
// Callers: deepgramDictation.ts.

const TARGET_RATE = 16000;
const CHUNK_MS = 40;

// Worklet processor: forwards raw Float32 mono frames to the main thread, which
// resamples to 16 kHz. Kept minimal so the audio render thread never stalls.
const WORKLET_SRC = `
class PcmForwarder extends AudioWorkletProcessor {
  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (ch && ch.length) this.port.postMessage(ch.slice(0));
    return true;
  }
}
registerProcessor('pcm-forwarder', PcmForwarder);
`;

export interface PcmCapture {
  start: (onChunk: (pcm16: Int16Array) => void) => Promise<void>;
  stop: () => void;
}

export function createPcmCapture(): PcmCapture {
  let ctx: AudioContext | null = null;
  let stream: MediaStream | null = null;
  let node: AudioWorkletNode | null = null;
  // eslint-disable-next-line @typescript-eslint/no-deprecated
  let proc: ScriptProcessorNode | null = null;
  let source: MediaStreamAudioSourceNode | null = null;
  let workletUrl: string | null = null;

  // Linear-interpolation resampler state (inputRate → 16 kHz). Fractional read
  // position carries across chunks so chunk boundaries don't drop/dup samples.
  let inRate = 48000;
  let readPos = 0;
  let pending: Float32Array[] = [];
  let pendingLen = 0;
  const samplesPerChunk = Math.round((TARGET_RATE * CHUNK_MS) / 1000); // 640

  const resampleEmit = (onChunk: (pcm16: Int16Array) => void) => {
    if (pendingLen === 0) return;
    const buf = new Float32Array(pendingLen);
    let off = 0;
    for (const p of pending) { buf.set(p, off); off += p.length; }
    pending = [];
    pendingLen = 0;

    const ratio = inRate / TARGET_RATE;
    const out: number[] = [];
    while (readPos < buf.length - 1) {
      const i = Math.floor(readPos);
      const frac = readPos - i;
      const s = buf[i]! * (1 - frac) + buf[i + 1]! * frac;
      out.push(Math.max(-32768, Math.min(32767, Math.round(s * 32767))));
      readPos += ratio;
    }
    readPos -= buf.length; // carry the fractional remainder into the next buffer

    for (let i = 0; i < out.length; i += samplesPerChunk) {
      onChunk(Int16Array.from(out.slice(i, i + samplesPerChunk)));
    }
  };

  const start = async (onChunk: (pcm16: Int16Array) => void) => {
    stream = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
    });
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const Ctx = window.AudioContext ?? (window as any).webkitAudioContext;
    ctx = new Ctx();
    inRate = ctx.sampleRate;
    readPos = 0;
    source = ctx.createMediaStreamSource(stream);

    const onFloat = (data: Float32Array) => {
      pending.push(data);
      pendingLen += data.length;
      if (pendingLen >= (inRate * CHUNK_MS) / 1000) resampleEmit(onChunk);
    };

    // Preferred path: AudioWorklet (off the main thread). Falls back to a
    // ScriptProcessorNode when the worklet can't load (older Safari, CSP, etc.)
    // so capture never silently dies.
    let workletOk = false;
    if (ctx.audioWorklet) {
      try {
        workletUrl = URL.createObjectURL(new Blob([WORKLET_SRC], { type: "application/javascript" }));
        await ctx.audioWorklet.addModule(workletUrl);
        node = new AudioWorkletNode(ctx, "pcm-forwarder");
        node.port.onmessage = (e: MessageEvent<Float32Array>) => onFloat(e.data);
        source.connect(node);
        workletOk = true;
      } catch {
        workletOk = false;
      }
    }
    if (!workletOk) {
      // eslint-disable-next-line @typescript-eslint/no-deprecated
      proc = ctx.createScriptProcessor(4096, 1, 1);
      proc.onaudioprocess = (e) => onFloat(new Float32Array(e.inputBuffer.getChannelData(0)));
      source.connect(proc);
      proc.connect(ctx.destination); // required to drive onaudioprocess; output stays silent
    }
  };

  const stop = () => {
    try { node?.port.close(); } catch { /* ignore */ }
    try { if (proc) proc.onaudioprocess = null; } catch { /* ignore */ }
    try { source?.disconnect(); } catch { /* ignore */ }
    try { node?.disconnect(); } catch { /* ignore */ }
    try { proc?.disconnect(); } catch { /* ignore */ }
    try { stream?.getTracks().forEach((t) => t.stop()); } catch { /* ignore */ }
    try { void ctx?.close(); } catch { /* ignore */ }
    if (workletUrl) { try { URL.revokeObjectURL(workletUrl); } catch { /* ignore */ } }
    ctx = null; stream = null; node = null; proc = null; source = null; workletUrl = null;
    pending = []; pendingLen = 0; readPos = 0;
  };

  return { start, stop };
}
