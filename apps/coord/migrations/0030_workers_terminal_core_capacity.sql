-- Latest authenticated worker terminal-core admission report. NULL is fail-closed
-- for pre-capacity workers and malformed reports; allocation itself remains local.

ALTER TABLE workers ADD COLUMN terminal_core_capacity_json TEXT;
