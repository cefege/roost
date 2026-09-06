// Central bounds for the worker's durable session-event SQLite outbox.
// Reservation admission, persisted-row validation, and database page limits
// all import these values so no layer silently permits a broader payload.

export const SESSION_EVENT_STORE_MAX_ROWS = 8_192;
export const SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES = 8 * 1024 * 1024;
export const SESSION_EVENT_STORE_MAX_DATABASE_BYTES = 16 * 1024 * 1024;
export const SESSION_EVENT_SEQUENCE_BLOCK_SIZE = 1_024;
