-- Static local display identity. NULL preserves generic platform fallback for
-- existing rows and workers that predate host-identity collection.

ALTER TABLE workers ADD COLUMN host_identity_json TEXT;
