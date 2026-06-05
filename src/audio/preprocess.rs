//! Pure audio preprocessing (KTD1/KTD2/KTD10).
//!
//! The audio thread emits native-rate, interleaved f32 samples. The
//! transcription worker calls [`to_canonical`] to downmix to mono and resample
//! to 16 kHz (the single canonical format both backends consume). The main
//! thread calls [`passes_gate`] on the *native* buffer before dispatch, so the
//! silence/duration check happens without paying for a resample on dropped clips.

use rubato::{FftFixedInOut, Resampler};

/// Whisper's required input rate.
const TARGET_RATE: usize = 16_000;
/// rubato FFT chunk size for the fixed-ratio resampler.
const RESAMPLE_CHUNK: usize = 1024;

/// Minimum clip duration; shorter presses are dropped (R4). An accidental tap
/// produces nothing useful and would still cost Groq's 10 s billing minimum.
const MIN_DURATION_MS: u64 = 350;
/// RMS below this is treated as silence (R4) to avoid whisper hallucinating
/// text from near-silent audio.
const SILENCE_RMS: f32 = 0.005;

/// i16 sample → f32 in [-1.0, 1.0).
pub fn i16_to_f32(sample: i16) -> f32 {
    sample as f32 / 32768.0
}

/// u16 sample (unsigned, centered at 32768) → f32 in [-1.0, 1.0).
pub fn u16_to_f32(sample: u16) -> f32 {
    (sample as f32 / 32768.0) - 1.0
}

/// Average interleaved channels into a mono buffer. Mono input passes through.
pub fn downmix_to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    if ch == 1 {
        return samples.to_vec();
    }
    samples
        .chunks_exact(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect()
}

/// Resample a mono buffer to 16 kHz with rubato's band-limited FFT resampler.
/// A no-op when already at 16 kHz. The final partial chunk is zero-padded.
pub fn resample_to_16k(mono: &[f32], src_rate: u32) -> Vec<f32> {
    if src_rate as usize == TARGET_RATE || mono.is_empty() {
        return mono.to_vec();
    }

    let mut resampler =
        match FftFixedInOut::<f32>::new(src_rate as usize, TARGET_RATE, RESAMPLE_CHUNK, 1) {
            Ok(r) => r,
            // Invalid ratio is not recoverable here; fall back to the raw buffer
            // so the backend still gets *something* rather than panicking the
            // worker. (Logged by the caller.)
            Err(_) => return mono.to_vec(),
        };

    let mut output =
        Vec::with_capacity(mono.len() * TARGET_RATE / src_rate as usize + RESAMPLE_CHUNK);
    let chunk = resampler.input_frames_next();
    let mut pos = 0;

    while pos + chunk <= mono.len() {
        if let Ok(out) = resampler.process(&[&mono[pos..pos + chunk]], None) {
            output.extend_from_slice(&out[0]);
        }
        pos += chunk;
    }

    if pos < mono.len() {
        let mut tail = vec![0.0f32; chunk];
        let remainder = &mono[pos..];
        tail[..remainder.len()].copy_from_slice(remainder);
        if let Ok(out) = resampler.process(&[&tail], None) {
            output.extend_from_slice(&out[0]);
        }
    }

    output
}

/// Downmix + resample native interleaved audio into the canonical 16 kHz mono
/// f32 buffer both backends consume.
pub fn to_canonical(samples: &[f32], src_rate: u32, channels: u16) -> Vec<f32> {
    let mono = downmix_to_mono(samples, channels);
    resample_to_16k(&mono, src_rate)
}

/// Whether a captured clip is long and loud enough to be worth transcribing (R4).
/// Operates on the native interleaved buffer (pre-resample).
pub fn passes_gate(samples: &[f32], rate: u32, channels: u16) -> bool {
    if samples.is_empty() || rate == 0 || channels == 0 {
        return false;
    }
    let frames = samples.len() / channels as usize;
    let duration_ms = (frames as u64 * 1000) / rate as u64;
    if duration_ms < MIN_DURATION_MS {
        return false;
    }
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    rms >= SILENCE_RMS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_stereo_to_half_length() {
        // 3 stereo frames: (L,R) pairs.
        let stereo = [1.0, 3.0, 0.0, 0.0, -1.0, 1.0];
        let mono = downmix_to_mono(&stereo, 2);
        assert_eq!(mono, vec![2.0, 0.0, 0.0]);
    }

    #[test]
    fn downmix_passes_mono_through_unchanged() {
        let mono = [0.1, -0.2, 0.3];
        assert_eq!(downmix_to_mono(&mono, 1), mono.to_vec());
    }

    #[test]
    fn int_conversions_map_full_scale_into_unit_range() {
        assert!((i16_to_f32(i16::MAX) - 0.999_97).abs() < 1e-3);
        assert!((i16_to_f32(i16::MIN) - -1.0).abs() < 1e-6);
        assert!((u16_to_f32(0) - -1.0).abs() < 1e-6);
        assert!((u16_to_f32(32768) - 0.0).abs() < 1e-6);
        assert!(u16_to_f32(u16::MAX) < 1.0 && u16_to_f32(u16::MAX) > 0.99);
    }

    #[test]
    fn resample_48k_to_16k_yields_about_one_third() {
        // 1 second of a 440 Hz sine at 48 kHz.
        let src_rate = 48_000;
        let n = src_rate;
        let sine: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / src_rate as f32).sin())
            .collect();
        let out = resample_to_16k(&sine, src_rate as u32);
        let expected = n / 3;
        // within one resampler chunk of the ideal 3:1 ratio
        assert!(
            (out.len() as i64 - expected as i64).abs() <= RESAMPLE_CHUNK as i64,
            "got {} samples, expected ~{expected}",
            out.len()
        );
        // energy is roughly preserved (not silenced/blown up by resampling)
        let rms = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        assert!(rms > 0.5 && rms < 0.8, "rms {rms}");
    }

    #[test]
    fn resample_is_noop_at_16k() {
        let buf = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_to_16k(&buf, 16_000), buf);
    }

    #[test]
    fn gate_rejects_short_clips() {
        // 100 ms mono at 16 kHz = 1600 samples, below the 350 ms floor.
        let short = vec![0.5f32; 1600];
        assert!(!passes_gate(&short, 16_000, 1));
    }

    #[test]
    fn gate_rejects_silence_even_when_long() {
        // 1 s of zeros at 16 kHz.
        let silent = vec![0.0f32; 16_000];
        assert!(!passes_gate(&silent, 16_000, 1));
    }

    #[test]
    fn gate_accepts_long_loud_clip() {
        let loud = vec![0.3f32; 16_000];
        assert!(passes_gate(&loud, 16_000, 1));
    }

    #[test]
    fn gate_accounts_for_channel_count() {
        // 1600 stereo frames = 3200 samples but only 100 ms — still too short.
        let short_stereo = vec![0.5f32; 3200];
        assert!(!passes_gate(&short_stereo, 16_000, 2));
    }

    #[test]
    fn gate_rejects_empty() {
        assert!(!passes_gate(&[], 16_000, 1));
    }
}
