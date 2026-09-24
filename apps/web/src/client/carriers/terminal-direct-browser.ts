// Browser-only direct-terminal primitives shared by loopback and RTC adapters.
// Adapters use these to keep request identities, monotonic timing, and
// RTCDataChannel buffer ownership consistent without sharing connection state.

export function createTerminalDirectRequestId(): string {
  if (globalThis.crypto?.randomUUID) return globalThis.crypto.randomUUID();
  const bytes = new Uint8Array(16);
  if (globalThis.crypto?.getRandomValues) globalThis.crypto.getRandomValues(bytes);
  else for (let index = 0; index < bytes.length; index += 1) bytes[index] = Math.floor(Math.random() * 256);
  bytes[6] = (bytes[6]! & 0x0f) | 0x40;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

export function terminalDirectMonotonicNow(): number {
  return globalThis.performance?.now() ?? Date.now();
}

/** Returns an ArrayBuffer the DOM BufferSource overload accepts on all targets. */
export function terminalDirectBufferSource(bytes: Uint8Array): ArrayBuffer {
  const buffer = bytes.buffer;
  if (
    buffer instanceof ArrayBuffer
    && bytes.byteOffset === 0
    && bytes.byteLength === buffer.byteLength
  ) return buffer;
  const copy = new Uint8Array(bytes.byteLength);
  copy.set(bytes);
  return copy.buffer;
}
