// Owns one outstanding ping and its pong deadline for a worker connection.
// makeWorkerConn supplies registry-generation and shutdown predicates so stale
// timer callbacks cannot close a replacement socket. Timer identity and ping
// generations jointly reject callbacks that were already cancelled.

import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
  DPingSchema,
  type CoordWorkerDown,
} from "@roost/shared/proto/worker_transport_pb";
import { log } from "@roost/shared/log";

export const WORKER_PING_DELAY_MS = 30_000;
export const WORKER_PONG_TIMEOUT_MS = 90_000;

interface WorkerConnKeepaliveOptions {
  isDone(): boolean;
  isCurrent(workerFp: string): boolean;
  sendBestEffort(what: string, frame: CoordWorkerDown): boolean;
  onPongTimeout(): void;
}

export interface WorkerConnKeepalive {
  stop(): void;
  scheduleNextPing(workerFp: string): void;
  acceptPong(workerFp: string, generation: bigint): void;
}

export function makeWorkerConnKeepalive(
  options: WorkerConnKeepaliveOptions,
): WorkerConnKeepalive {
  let nextPingTimer: Timer | null = null;
  let pongDeadlineTimer: Timer | null = null;
  let outstandingPingGeneration: bigint | null = null;
  let nextPingGeneration = 0n;

  function stop(): void {
    if (nextPingTimer) {
      clearTimeout(nextPingTimer);
      nextPingTimer = null;
    }
    if (pongDeadlineTimer) {
      clearTimeout(pongDeadlineTimer);
      pongDeadlineTimer = null;
    }
    outstandingPingGeneration = null;
  }

  function scheduleNextPing(workerFp: string): void {
    if (
      options.isDone()
      || !options.isCurrent(workerFp)
      || outstandingPingGeneration !== null
      || nextPingTimer !== null
      || pongDeadlineTimer !== null
    ) return;
    const timer = setTimeout(() => {
      // A cleared callback that was already runnable must not erase or replace
      // a newer timer scheduled by an exact pong.
      if (nextPingTimer !== timer) return;
      nextPingTimer = null;
      if (options.isDone() || !options.isCurrent(workerFp)) return;

      nextPingGeneration += 1n;
      const generation = nextPingGeneration;
      outstandingPingGeneration = generation;
      options.sendBestEffort("keepalive", create(CoordWorkerDownSchema, {
        frame: {
          case: "ping",
          value: create(DPingSchema, { ts: generation }),
        },
      }));

      const deadline = setTimeout(() => {
        // Both identities matter: neither a cancelled callback nor a
        // superseded socket may close the current worker connection.
        if (pongDeadlineTimer !== deadline) return;
        if (
          options.isDone()
          || !options.isCurrent(workerFp)
          || outstandingPingGeneration !== generation
        ) return;
        pongDeadlineTimer = null;
        log.warn("worker-service", "pong_timeout", {
          worker_fp: workerFp,
          ping_generation: generation.toString(),
        });
        options.onPongTimeout();
      }, WORKER_PONG_TIMEOUT_MS);
      pongDeadlineTimer = deadline;
    }, WORKER_PING_DELAY_MS);
    nextPingTimer = timer;
  }

  function acceptPong(workerFp: string, generation: bigint): void {
    if (
      !options.isCurrent(workerFp)
      || outstandingPingGeneration !== generation
    ) return;
    outstandingPingGeneration = null;
    if (pongDeadlineTimer !== null) {
      clearTimeout(pongDeadlineTimer);
      pongDeadlineTimer = null;
    }
    scheduleNextPing(workerFp);
  }

  return { stop, scheduleNextPing, acceptPong };
}
