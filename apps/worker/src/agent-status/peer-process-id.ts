// Reads the kernel-attested process ID for one accepted local report socket.
// Bun's private node:net handle access and platform FFI stay isolated here;
// report-server consumes only the fail-closed reader interface. Tests inject
// the native query while exercising the same handle and PID validation.
import type { Socket } from "node:net";
import { dlopen, FFIType, ptr, type Pointer } from "bun:ffi";
import {
  assertNeverPlatform,
  supportedHostPlatform,
  type SupportedHostPlatform,
} from "@roost/shared/platform";

export interface NativePeerProcessIdQuery {
  read(nativeHandle: number): number | null;
  close(): void;
}

export interface LocalPeerProcessIdReader {
  readonly available: boolean;
  read(socket: Socket): number | null;
  close(): void;
}

export interface LocalPeerProcessIdReaderOptions {
  platform?: SupportedHostPlatform;
  nativeQuery?: NativePeerProcessIdQuery;
}

interface BunAcceptedSocket extends Socket {
  _handle?: {
    fd?: unknown;
    handle?: unknown;
  };
}

const GETSOCKOPT_SIGNATURE = {
  args: [FFIType.i32, FFIType.i32, FFIType.i32, FFIType.ptr, FFIType.ptr],
  returns: FFIType.i32,
} as const;

function nativeSocketHandles(
  socket: Socket,
  platform: SupportedHostPlatform,
): number[] {
  const handle = (socket as BunAcceptedSocket)._handle;
  const candidates = platform === "win32"
    ? [handle?.handle]
    : [handle?.fd];
  const nativeHandles: number[] = [];
  for (const candidate of candidates) {
    let nativeHandle: number | null = null;
    if (typeof candidate === "number") {
      if (Number.isSafeInteger(candidate) && candidate >= 0) nativeHandle = candidate;
    } else if (
      typeof candidate === "bigint"
      && candidate >= 0n
      && candidate <= BigInt(Number.MAX_SAFE_INTEGER)
    ) {
      nativeHandle = Number(candidate);
    }
    if (nativeHandle !== null && !nativeHandles.includes(nativeHandle)) {
      nativeHandles.push(nativeHandle);
    }
  }
  return nativeHandles;
}

function openLinuxQuery(): NativePeerProcessIdQuery {
  const library = dlopen("libc.so.6", { getsockopt: GETSOCKOPT_SIGNATURE });
  const credentials = new Int32Array(3);
  const length = new Uint32Array(1);
  return {
    read(socketDescriptor) {
      length[0] = credentials.byteLength;
      const result = library.symbols.getsockopt(
        socketDescriptor,
        1,
        17,
        ptr(credentials),
        ptr(length),
      );
      return result === 0 && length[0] >= credentials.byteLength ? credentials[0]! : null;
    },
    close: () => { library.close(); },
  };
}

function openDarwinQuery(): NativePeerProcessIdQuery {
  const library = dlopen("/usr/lib/libSystem.B.dylib", {
    getsockopt: GETSOCKOPT_SIGNATURE,
  });
  const processId = new Int32Array(1);
  const length = new Uint32Array(1);
  return {
    read(socketDescriptor) {
      length[0] = processId.byteLength;
      const result = library.symbols.getsockopt(
        socketDescriptor,
        0,
        2,
        ptr(processId),
        ptr(length),
      );
      return result === 0 && length[0] === processId.byteLength ? processId[0]! : null;
    },
    close: () => { library.close(); },
  };
}

function openWindowsQuery(): NativePeerProcessIdQuery {
  const library = dlopen("kernel32.dll", {
    GetNamedPipeClientProcessId: {
      args: [FFIType.ptr, FFIType.ptr],
      returns: FFIType.i32,
    },
  });
  const processId = new Uint32Array(1);
  return {
    read(pipeHandle) {
      processId[0] = 0;
      const result = library.symbols.GetNamedPipeClientProcessId(
        pipeHandle as Pointer,
        ptr(processId),
      );
      return result !== 0 ? processId[0]! : null;
    },
    close: () => { library.close(); },
  };
}

function openNativeQuery(
  platform: SupportedHostPlatform,
): NativePeerProcessIdQuery | null {
  try {
    switch (platform) {
      case "linux": return openLinuxQuery();
      case "darwin": return openDarwinQuery();
      case "win32": return openWindowsQuery();
      default: return assertNeverPlatform(platform);
    }
  } catch {
    return null;
  }
}

export function createLocalPeerProcessIdReader(
  options: LocalPeerProcessIdReaderOptions = {},
): LocalPeerProcessIdReader {
  const platform = options.platform ?? supportedHostPlatform();
  const nativeQuery = options.nativeQuery ?? openNativeQuery(platform);
  return {
    available: nativeQuery !== null,
    read(socket) {
      if (!nativeQuery) return null;
      for (const nativeHandle of nativeSocketHandles(socket, platform)) {
        try {
          const processId = nativeQuery.read(nativeHandle);
          if (
            processId !== null
            && Number.isSafeInteger(processId)
            && processId > 0
          ) return processId;
        } catch {
          // Try the next platform handle shape, then fail closed.
        }
      }
      return null;
    },
    close() {
      nativeQuery?.close();
    },
  };
}
