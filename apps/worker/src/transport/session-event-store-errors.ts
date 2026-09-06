// Errors owned by the durable worker session-event outbox.
// The store, schema migration, event sink, and mutation admission callers use
// these classes to distinguish capacity rejection from fatal durability loss.

export class SessionEventOutboxFullError extends Error {
  constructor() {
    super("session event outbox full");
    this.name = "SessionEventOutboxFullError";
  }
}

export class SessionEventStoreFatalError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "SessionEventStoreFatalError";
  }
}

export function sessionEventStoreFatal(
  message: string,
  cause?: unknown,
): SessionEventStoreFatalError {
  return new SessionEventStoreFatalError(
    message,
    cause === undefined ? undefined : { cause },
  );
}
