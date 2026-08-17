import { randomBytes } from "node:crypto";
import net, { type Server, type Socket } from "node:net";
import {
  cleanupLocalEndpoint,
  LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS,
  LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT_MS,
  prepareLocalEndpoint,
  resolveLocalEndpoint,
  secureLocalEndpoint,
  verifyLocalEndpointCapability,
  type LocalEndpoint,
} from "./local-endpoint.ts";
import { runWindowsHelper } from "./windows-helper.ts";
import { coordDataDir, workerDataDir } from "./paths.ts";
import {
  SERVICE_HEALTH_PROTOCOL_VERSION,
  ServiceHealthFrameAccumulator,
  ServiceHealthProbeCapabilitySchema,
  ServiceHealthProbeRequestSchema,
  ServiceHealthProbeResponseSchema,
  decodeServiceHealthFrame,
  encodeServiceHealthFrame,
  validateServiceHealthStatus,
  type CoordinatorServiceHealth,
  type ServiceHealthFrameProgress,
  type ServiceHealthProbeRequest,
  type ServiceHealthProbeResponse,
  type ServiceHealthRole,
  type ServiceHealthStatusFor,
  type WorkerServiceHealth,
} from "./service-health-protocol.ts";

export {
  CoordinatorServiceHealthSchema,
  WorkerServiceHealthSchema,
  type CoordinatorServiceHealth,
  type ServiceHealthCommon,
  type ServiceHealthRole,
  type ServiceHealthStatus,
  type ServiceHealthStatusFor,
  type WorkerServiceHealth,
} from "./service-health-protocol.ts";

export interface ServeServiceHealthOptions {
  dataDir?: string;
  endpoint?: LocalEndpoint;
  /** May shorten, but never extend, the complete connection deadline. */
  timeoutMs?: number;
}

export interface ServiceHealthProbeOptions {
  dataDir?: string;
  endpoint?: LocalEndpoint;
  /** May shorten, but never extend, the complete probe deadline. */
  timeoutMs?: number;
  expectedVersion?: string;
  expectedBuild?: string;
  previousProcessEpoch?: string;
  expectedCoordinatorUrl?: string;
}

export interface ServiceHealthServer {
  endpoint: LocalEndpoint;
  close(): Promise<void>;
}

function assertRole(role: unknown): asserts role is ServiceHealthRole {
  if (role !== "coordinator" && role !== "worker") throw new Error("invalid service health role");
}

function connectionTimeout(timeoutMs: number | undefined): number {
  const resolved = timeoutMs ?? LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT_MS;
  if (
    !Number.isInteger(resolved)
    || resolved <= 0
    || resolved > LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT_MS
  ) {
    throw new RangeError(
      `service health timeout must be between 1 and ${LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT_MS}ms`,
    );
  }
  return resolved;
}

function endpointFor(
  role: ServiceHealthRole,
  options: Pick<ServeServiceHealthOptions, "dataDir" | "endpoint">,
): LocalEndpoint {
  return options.endpoint ?? resolveLocalEndpoint({
    name: `${role}-health`,
    dataDir: options.dataDir ?? (
      role === "coordinator" ? coordDataDir() : workerDataDir()
    ),
  });
}

function parseAuthenticatedRequest(
  role: ServiceHealthRole,
  endpoint: LocalEndpoint,
  payload: Buffer,
): ServiceHealthProbeRequest | null {
  let value: unknown;
  try {
    value = decodeServiceHealthFrame(payload);
  } catch {
    return null;
  }
  const authentication = ServiceHealthProbeCapabilitySchema.safeParse(value);
  if (
    !authentication.success
    || !verifyLocalEndpointCapability(endpoint.capability, authentication.data.capability)
  ) {
    return null;
  }
  const request = ServiceHealthProbeRequestSchema.safeParse(value);
  return request.success && request.data.role === role ? request.data : null;
}

function closeServer(server: Server, sockets: Set<Socket>): Promise<void> {
  const { promise, resolve, reject } = Promise.withResolvers<void>();
  if (!server.listening) {
    for (const socket of sockets) socket.destroy();
    resolve();
    return promise;
  }
  server.close((error) => error ? reject(error) : resolve());
  for (const socket of sockets) socket.destroy();
  return promise;
}

async function protectWindowsHealthCapability(
  role: ServiceHealthRole,
  endpoint: LocalEndpoint,
): Promise<void> {
  if (endpoint.platform !== "win32") return;
  await runWindowsHelper<{ ok: true }>(
    "protect-service-health",
    [endpoint.capabilityPath, role],
  );
}

export async function serveServiceHealth<R extends ServiceHealthRole>(
  role: R,
  getStatus: () => ServiceHealthStatusFor<R> | Promise<ServiceHealthStatusFor<R>>,
  options: ServeServiceHealthOptions = {},
): Promise<ServiceHealthServer> {
  assertRole(role);
  const timeoutMs = connectionTimeout(options.timeoutMs);
  const endpoint = endpointFor(role, options);
  await prepareLocalEndpoint(endpoint);
  await protectWindowsHealthCapability(role, endpoint);

  let unauthenticatedConnections = 0;
  const sockets = new Set<Socket>();
  const server = net.createServer((socket) => {
    sockets.add(socket);
    if (unauthenticatedConnections >= LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS) {
      socket.destroy();
      return;
    }
    unauthenticatedConnections++;
    let awaitingAuthentication = true;
    let processing = false;
    const frame = new ServiceHealthFrameAccumulator();
    const releaseUnauthenticatedSlot = () => {
      if (!awaitingAuthentication) return;
      awaitingAuthentication = false;
      unauthenticatedConnections = Math.max(0, unauthenticatedConnections - 1);
    };
    const timer = setTimeout(() => socket.destroy(), timeoutMs);
    timer.unref?.();

    socket.once("close", () => {
      clearTimeout(timer);
      releaseUnauthenticatedSlot();
      sockets.delete(socket);
    });
    socket.on("error", () => undefined);
    socket.on("data", (chunk: Buffer) => {
      if (processing) {
        socket.destroy();
        return;
      }
      let result: ServiceHealthFrameProgress;
      try {
        result = frame.push(chunk);
      } catch {
        socket.destroy();
        return;
      }
      if (!result.complete) return;
      processing = true;
      const request = parseAuthenticatedRequest(role, endpoint, result.payload);
      if (!request) {
        socket.destroy();
        return;
      }
      releaseUnauthenticatedSlot();

      void (async () => {
        try {
          const status = validateServiceHealthStatus(role, await getStatus());
          const response: ServiceHealthProbeResponse = {
            protocolVersion: SERVICE_HEALTH_PROTOCOL_VERSION,
            kind: "status",
            role,
            generation: request.generation,
            status,
          };
          if (!socket.destroyed && socket.writable) {
            socket.end(encodeServiceHealthFrame(response));
          }
        } catch {
          socket.destroy();
        }
      })();
    });
  });
  server.maxConnections = LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS;

  try {
    const { promise, resolve, reject } = Promise.withResolvers<void>();
    const onError = (error: Error) => reject(error);
    server.once("error", onError);
    server.listen(endpoint.address, () => {
      server.removeListener("error", onError);
      resolve();
    });
    await promise;
    server.on("error", () => {
      for (const socket of sockets) socket.destroy();
    });
    if (endpoint.platform !== "win32") await secureLocalEndpoint(endpoint);
  } catch (error) {
    try {
      await closeServer(server, sockets);
    } catch {
      // Preserve the listen/secure error.
    }
    await cleanupLocalEndpoint(endpoint);
    throw error;
  }

  let closePromise: Promise<void> | undefined;
  return {
    endpoint,
    close(): Promise<void> {
      closePromise ??= (async () => {
        try {
          await closeServer(server, sockets);
        } finally {
          await cleanupLocalEndpoint(endpoint);
        }
      })();
      return closePromise;
    },
  };
}

function readProbeResponse(
  endpoint: LocalEndpoint,
  request: ServiceHealthProbeRequest,
  timeoutMs: number,
): Promise<Buffer> {
  const { promise, resolve, reject } = Promise.withResolvers<Buffer>();
  const socket = net.createConnection(endpoint.address);
  const frame = new ServiceHealthFrameAccumulator();
  let payload: Buffer | undefined;
  let settled = false;
  function fail(error: Error) {
    if (settled) return;
    settled = true;
    clearTimeout(timer);
    socket.destroy();
    reject(error);
  }
  const timer = setTimeout(() => fail(new Error("service health probe timed out")), timeoutMs);
  timer.unref?.();

  socket.on("error", (error) => fail(error));
  socket.on("connect", () => {
    try {
      socket.write(encodeServiceHealthFrame(request));
    } catch (error) {
      fail(error instanceof Error ? error : new Error(String(error)));
    }
  });
  socket.on("data", (chunk: Buffer) => {
    try {
      const result = frame.push(chunk);
      if (result.complete) payload = result.payload;
    } catch (error) {
      fail(error instanceof Error ? error : new Error(String(error)));
    }
  });
  socket.on("end", () => {
    if (settled) return;
    if (!frame.complete || !payload) {
      fail(new Error("service health probe received an incomplete response"));
      return;
    }
    settled = true;
    clearTimeout(timer);
    resolve(payload);
  });
  socket.on("close", () => {
    if (!settled) fail(new Error("service health endpoint closed without a response"));
  });
  return promise;
}

async function readAttestedWindowsProbeResponse(
  role: ServiceHealthRole,
  endpoint: LocalEndpoint,
  request: ServiceHealthProbeRequest,
  timeoutMs: number,
): Promise<Buffer> {
  const service = role === "worker" ? "RoostWorkerV2" : "RoostCoordinatorV2";
  const input = new TextEncoder().encode(JSON.stringify(request));
  const result = await runWindowsHelper<{ serverPid: number; payloadUtf8: string }>(
    "probe-service-health",
    [service, endpoint.address],
    { input, timeoutMs },
  );
  if (
    !Number.isInteger(result.serverPid)
    || result.serverPid <= 0
    || typeof result.payloadUtf8 !== "string"
    || Buffer.byteLength(result.payloadUtf8) > 64 * 1024
  ) {
    throw new Error("native Windows service health attestation returned invalid data");
  }
  return Buffer.from(result.payloadUtf8, "utf8");
}

function parsedProbeResponse<R extends ServiceHealthRole>(
  role: R,
  generation: string,
  payload: Buffer,
  options: ServiceHealthProbeOptions,
): ServiceHealthStatusFor<R> {
  let value: unknown;
  try {
    value = decodeServiceHealthFrame(payload);
  } catch {
    throw new Error("service health probe received malformed JSON");
  }
  const response = ServiceHealthProbeResponseSchema.safeParse(value);
  if (
    !response.success
    || response.data.role !== role
    || response.data.generation !== generation
  ) {
    throw new Error("service health probe received a stale or invalid response");
  }
  const status = validateServiceHealthStatus(role, response.data.status);
  if (!status.ready) throw new Error(`${role} service is not ready`);
  if (role === "coordinator") {
    const coordinator = status as CoordinatorServiceHealth;
    if (!coordinator.dbReady || !coordinator.listenerReady) {
      throw new Error("coordinator service is not ready");
    }
  } else {
    const worker = status as WorkerServiceHealth;
    if (!worker.targetLinkReady) throw new Error("worker target link is not ready");
    if (
      options.expectedCoordinatorUrl !== undefined
      && worker.coordinatorUrl !== options.expectedCoordinatorUrl
    ) {
      throw new Error("worker is connected to the wrong coordinator target");
    }
  }
  if (options.expectedVersion !== undefined && status.version !== options.expectedVersion) {
    throw new Error("service health version does not match");
  }
  if (options.expectedBuild !== undefined && status.build !== options.expectedBuild) {
    throw new Error("service health build does not match");
  }
  if (
    options.previousProcessEpoch !== undefined
    && status.processEpoch === options.previousProcessEpoch
  ) {
    throw new Error("service health response came from the previous process epoch");
  }
  return status;
}

export async function probeServiceHealth<R extends ServiceHealthRole>(
  role: R,
  options: ServiceHealthProbeOptions = {},
): Promise<ServiceHealthStatusFor<R>> {
  assertRole(role);
  const timeoutMs = connectionTimeout(options.timeoutMs);
  if (role === "coordinator" && options.expectedCoordinatorUrl !== undefined) {
    throw new Error("expectedCoordinatorUrl is valid only for worker health probes");
  }
  const endpoint = endpointFor(role, options);
  const generation = randomBytes(32).toString("hex");
  const request: ServiceHealthProbeRequest = {
    protocolVersion: SERVICE_HEALTH_PROTOCOL_VERSION,
    kind: "probe",
    role,
    generation,
    capability: endpoint.capability,
  };
  const payload = endpoint.platform === "win32"
    ? await readAttestedWindowsProbeResponse(role, endpoint, request, timeoutMs)
    : await readProbeResponse(endpoint, request, timeoutMs);
  return parsedProbeResponse(role, generation, payload, options);
}
