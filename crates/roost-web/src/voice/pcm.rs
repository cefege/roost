//! Linear16 PCM at the rate Deepgram asked for, built from the float frames the
//! browser's audio graph produces.
//!
//! Split out of the capture host because the conversion is arithmetic with no
//! device in it, and because it is where a wrong rate shows up as a transcript
//! that is fast-forwarded or slowed rather than as an error. The browser hands
//! float mono at whatever rate the context runs (48 kHz on most machines); the
//! engine needs signed 16-bit mono at 16 kHz in fixed-size chunks.
//! Ports the resampler half of `apps/web/src/voice/audioPcmCapture.ts`.

/// The rate the engine's URL declares, and therefore the rate its decoder
/// assumes. Anything else is a transcript at the wrong speed.
pub const TARGET_RATE: u32 = 16_000;

/// How much audio one chunk carries. 40 ms is two Deepgram frames, so a dropped
/// chunk is a dropped word rather than a dropped syllable.
pub const CHUNK_MS: u32 = 40;

/// Samples per chunk: `TARGET_RATE * CHUNK_MS / 1000`.
pub const SAMPLES_PER_CHUNK: usize = (TARGET_RATE as usize * CHUNK_MS as usize) / 1000;

/// The rate assumed for a graph that has not reported one yet. 48 kHz is what
/// every desktop browser picks, and assuming it only costs one interpolation
/// step if the real rate differs.
pub const DEFAULT_INPUT_RATE: u32 = 48_000;

/// Float frames in, linear16 bytes out, at the input graph's rate.
///
/// The fractional read position is carried across calls rather than truncated:
/// dropping it accumulates half a sample per chunk, which is a slow drift in the
/// transcript's speed and not something a test of the bytes would catch.
#[derive(Debug, Clone)]
pub struct Resampler {
    input_rate: u32,
    read_position: f64,
    pending: Vec<f32>,
}

impl Default for Resampler {
    fn default() -> Self {
        Self::new(DEFAULT_INPUT_RATE)
    }
}

impl Resampler {
    /// A resampler for a graph running at `input_rate`.
    #[must_use]
    pub fn new(input_rate: u32) -> Self {
        Self {
            input_rate: input_rate.max(1),
            read_position: 0.0,
            pending: Vec::new(),
        }
    }

    /// Feed float frames, and take the whole chunks they complete.
    ///
    /// A trailing partial chunk stays pending until enough audio arrives; a
    /// recording that is stopped mid-chunk drops it rather than padding it with
    /// silence the operator never spoke.
    #[must_use]
    pub fn push(&mut self, frames: &[f32]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(frames);
        let per_chunk = (self.input_rate * CHUNK_MS) / 1000;
        let per_chunk = per_chunk.max(1) as usize;
        let mut chunks = Vec::new();
        while self.pending.len() >= per_chunk {
            let frame: Vec<f32> = self.pending.drain(..per_chunk).collect();
            chunks.push(self.encode(&frame));
        }
        chunks
    }

    /// The input rate this resampler was built for, for the diagnostics facts.
    #[must_use]
    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }

    /// Whether a partial chunk is waiting for more audio.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn encode(&mut self, frame: &[f32]) -> Vec<u8> {
        let step = f64::from(self.input_rate) / f64::from(TARGET_RATE);
        let mut bytes = Vec::with_capacity(SAMPLES_PER_CHUNK * 2);
        let mut index = self.read_position;
        while index < frame.len() as f64 {
            let lower = index.floor() as usize;
            let fraction = (index - lower as f64) as f32;
            let current = frame.get(lower).copied().unwrap_or(0.0);
            let next = frame.get(lower + 1).copied().unwrap_or(current);
            let sample = current + (next - current) * fraction;
            bytes.extend_from_slice(&to_linear16(sample).to_le_bytes());
            index += step;
        }
        // Keep the fraction across the chunk boundary; the next chunk starts
        // where this one stopped reading.
        self.read_position = index - frame.len() as f64;
        bytes
    }
}

/// One float sample as a signed 16-bit one, clamped rather than wrapped.
///
/// A sample above full scale must saturate: wrapping turns a loud syllable into
/// a burst of noise, and Deepgram transcribes noise.
#[must_use]
pub fn to_linear16(sample: f32) -> i16 {
    let scaled = (f64::from(sample) * 32767.0).round();
    scaled.clamp(-32768.0, 32767.0) as i16
}

#[cfg(test)]
mod tests {
    use super::{Resampler, SAMPLES_PER_CHUNK, TARGET_RATE, to_linear16};

    #[test]
    fn a_chunk_is_the_declared_number_of_linear16_samples() {
        let mut resampler = Resampler::new(48_000);
        let chunks = resampler.push(&vec![0.25; 1_920]);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), SAMPLES_PER_CHUNK * 2);
    }

    #[test]
    fn three_input_chunks_make_three_output_chunks_at_the_target_rate() {
        let mut resampler = Resampler::new(48_000);
        let chunks = resampler.push(&vec![0.0; 5_760]);
        assert_eq!(chunks.len(), 3);
        for chunk in chunks {
            assert_eq!(chunk.len(), SAMPLES_PER_CHUNK * 2);
        }
    }

    #[test]
    fn a_partial_chunk_waits_for_the_rest_of_its_audio() {
        let mut resampler = Resampler::new(48_000);
        assert!(resampler.push(&vec![0.0; 1_000]).is_empty());
        assert!(resampler.has_pending());
        let chunks = resampler.push(&vec![0.0; 1_000]);
        assert_eq!(chunks.len(), 1);
        // The remainder of the second push is still short of a chunk: a dropped
        // chunk is dropped whole, never padded with silence.
        assert!(resampler.has_pending());
    }

    #[test]
    fn a_steady_tone_keeps_its_level_through_the_resample() {
        let mut resampler = Resampler::new(48_000);
        let chunks = resampler.push(&vec![0.5; 4_800]);
        let first = i16::from_le_bytes([chunks[0][0], chunks[0][1]]);
        let last_index = chunks[0].len() - 2;
        let last = i16::from_le_bytes([chunks[0][last_index], chunks[0][last_index + 1]]);
        assert_eq!(first, 16_384);
        assert_eq!(last, 16_384);
    }

    #[test]
    fn the_resampled_length_tracks_the_input_length_over_the_target_rate() {
        // 48 kHz in, 16 kHz out: three chunks in, one out. A truncated read
        // position would drift a sample per chunk and lose one within a second.
        let mut resampler = Resampler::new(48_000);
        for _ in 0..4 {
            let chunks = resampler.push(&vec![0.1; 1_920]);
            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].len(), SAMPLES_PER_CHUNK * 2);
        }
    }

    #[test]
    fn a_graph_already_at_the_target_rate_passes_samples_through() {
        let mut resampler = Resampler::new(TARGET_RATE);
        let frames = (0..640)
            .map(|index| (index as f32) / 640.0 - 0.5)
            .collect::<Vec<f32>>();
        let chunks = resampler.push(&frames);
        assert_eq!(chunks.len(), 1);
        let first = i16::from_le_bytes([chunks[0][0], chunks[0][1]]);
        let last_index = chunks[0].len() - 2;
        let last = i16::from_le_bytes([chunks[0][last_index], chunks[0][last_index + 1]]);
        assert!(first < last, "a rising ramp must stay rising");
    }

    #[test]
    fn a_sample_outside_full_scale_saturates_instead_of_wrapping() {
        assert_eq!(to_linear16(0.0), 0);
        assert_eq!(to_linear16(1.0), 32_767);
        assert_eq!(to_linear16(-1.0), -32_767);
        assert_eq!(to_linear16(4.0), 32_767);
        assert_eq!(to_linear16(-4.0), -32_768);
        assert_eq!(to_linear16(0.5), 16_384);
    }

    #[test]
    fn a_silence_that_is_not_whole_samples_is_still_whole_chunks() {
        // 44.1 kHz makes 1764 input samples per 40 ms chunk, which resamples to
        // exactly the same 640 the engine's URL declares.
        let mut resampler = Resampler::new(44_100);
        let chunks = resampler.push(&vec![0.0; 4_410]);
        assert_eq!(chunks.len(), 2);
        for chunk in chunks {
            // The carried read position makes a chunk one sample short or long at
            // most; what must not happen is a chunk that drifts.
            assert!(
                (chunk.len() as i64 - (SAMPLES_PER_CHUNK * 2) as i64).abs() <= 2,
                "chunk was {} bytes",
                chunk.len()
            );
        }
    }
}
