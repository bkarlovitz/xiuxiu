//! Local whisper.cpp backend via `whisper-rs` (R6, R9).
//!
//! The model (`WhisperContext`) is loaded eagerly at construction (`try_new`)
//! and held behind an `Arc`, so it loads once at startup and never reloads on a
//! backend switch (KTD5). `transcribe` creates a cheap per-utterance
//! `WhisperState` and runs the synchronous `full()` on the worker thread.

use std::path::Path;
use std::sync::Arc;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::TranscribeError;

#[derive(Clone)]
pub struct LocalWhisperBackend {
    ctx: Arc<WhisperContext>,
}

impl LocalWhisperBackend {
    /// Load the ggml model. Returns a typed error (not a panic) on a missing or
    /// unreadable model so the caller can fall back to Groq (R14).
    pub fn try_new(model_path: &Path) -> Result<Self, TranscribeError> {
        let path = model_path
            .to_str()
            .ok_or_else(|| TranscribeError::Local("model path is not valid UTF-8".to_string()))?;
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(|e| TranscribeError::Local(format!("failed to load model: {e}")))?;
        Ok(LocalWhisperBackend { ctx: Arc::new(ctx) })
    }

    pub fn transcribe(&self, audio: &[f32]) -> Result<String, TranscribeError> {
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| TranscribeError::Local(e.to_string()))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        // whisper.cpp prints to stdout/stderr by default — pointless and noisy
        // under windows_subsystem="windows" (KTD8). Silence all of it.
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_n_threads(thread_count());

        state
            .full(params, audio)
            .map_err(|e| TranscribeError::Local(e.to_string()))?;

        let n = state
            .full_n_segments()
            .map_err(|e| TranscribeError::Local(e.to_string()))?;
        let mut segments = Vec::with_capacity(n as usize);
        for i in 0..n {
            let seg = state
                .full_get_segment_text(i)
                .map_err(|e| TranscribeError::Local(e.to_string()))?;
            segments.push(seg);
        }
        Ok(concat_segments(&segments))
    }
}

fn thread_count() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8) as i32
}

/// Join whisper segments into one trimmed string. Pure so it is testable
/// without a model.
fn concat_segments(segments: &[String]) -> String {
    segments.join("").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concat_joins_and_trims_segments() {
        let segs = vec![
            " Hello".to_string(),
            " there,".to_string(),
            " world. ".to_string(),
        ];
        assert_eq!(concat_segments(&segs), "Hello there, world.");
    }

    #[test]
    fn concat_handles_empty() {
        assert_eq!(concat_segments(&[]), "");
    }

    #[test]
    fn try_new_on_missing_model_returns_error_not_panic() {
        let result = LocalWhisperBackend::try_new(Path::new("/definitely/not/a/model.bin"));
        assert!(matches!(result, Err(TranscribeError::Local(_))));
    }
}
