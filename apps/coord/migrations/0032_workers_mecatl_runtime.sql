-- Latest worker-reported Mecatl runtime state for this machine. NULL means the
-- worker never reported one, which reads as unknown rather than disabled: only
-- a worker that ran this code can distinguish an opted-out machine from a
-- crashed daemon.

ALTER TABLE workers ADD COLUMN mecatl_runtime_json TEXT;
