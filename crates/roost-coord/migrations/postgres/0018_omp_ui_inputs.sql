ALTER TABLE omp_inputs DROP CONSTRAINT omp_inputs_kind_check;
ALTER TABLE omp_inputs ADD CONSTRAINT omp_inputs_kind_check CHECK (kind IN ('prompt', 'abort', 'respond', 'ui'));
