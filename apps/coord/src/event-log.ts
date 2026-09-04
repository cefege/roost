// This module keeps the coordinator's stable event-log import surface.
// Session handlers, worker connections, and sync feeds import APIs from here.
// Query, projection, and append implementations live in focused sibling modules.
// Re-exports must preserve durable transaction and publication semantics.

export {
  appendEvent,
  dispatchSnapshotOrphanReaps,
  type AppendEventResult,
} from "./event-transaction.ts";
export { SESSION_COLUMNS } from "./event-projection.ts";
export {
  getEventMaxId,
  getEventsSince,
  getEventsThrough,
} from "./event-query.ts";
