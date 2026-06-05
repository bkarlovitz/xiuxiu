//! Transcription backends and dispatch (KTD6).
//!
//! `ActiveBackend` is an enum, not a `dyn` trait: the two backends have
//! divergent execution models (Groq async, local sync-blocking) so a shared
//! `async` trait would force `async_trait` + object-safety friction for two
//! known branches. The enum matches on the worker thread and both arms return
//! `Result<String, TranscribeError>`, keeping the injection path backend-agnostic.
//!
//! [`Backends`] holds both backends (each built once — the local model stays
//! resident, KTD5) and owns the startup-fallback and runtime-switch decisions
//! (R14). The pure decision helpers ([`resolve_initial`], [`resolve_switch`])
//! are unit-tested without needing a real model.

pub mod groq;
pub mod local;

use thiserror::Error;

use crate::config::{Backend, RawConfig};
use groq::GroqBackend;
use local::LocalWhisperBackend;

/// Errors from a transcription attempt. None of these variants carry the API
/// key, so the derived `Debug` cannot leak it (R16).
#[derive(Debug, Error)]
pub enum TranscribeError {
    #[error("network error: {0}")]
    Network(String),
    #[error("request timed out")]
    Timeout,
    #[error("authentication failed — check GROQ_API_KEY: {0}")]
    Auth(String),
    #[error("rate limited{}", .retry_after.map(|s| format!(" — retry after {s}s")).unwrap_or_default())]
    RateLimited {
        retry_after: Option<u64>,
        message: String,
    },
    #[error("audio too large ({0} bytes)")]
    TooLarge(usize),
    #[error("server error: {0}")]
    Server(String),
    #[error("unexpected response ({0}): {1}")]
    Http(u16, String),
    #[error("failed to decode response: {0}")]
    Decode(String),
    #[error("failed to encode audio: {0}")]
    Encode(String),
    #[error("local model error: {0}")]
    Local(String),
}

/// Fatal startup configuration errors — no usable backend can be constructed.
#[derive(Debug, Error)]
pub enum FatalError {
    #[error("BACKEND=groq but no GROQ_API_KEY is set")]
    GroqSelectedNoKey,
    #[error("BACKEND=local but the model could not be loaded and no GROQ_API_KEY is set to fall back to")]
    LocalSelectedUnavailableNoFallback,
}

/// The active transcription backend.
#[derive(Clone)]
pub enum ActiveBackend {
    Groq(GroqBackend),
    Local(LocalWhisperBackend),
}

impl ActiveBackend {
    pub fn kind(&self) -> Backend {
        match self {
            ActiveBackend::Groq(_) => Backend::Groq,
            ActiveBackend::Local(_) => Backend::Local,
        }
    }

    /// Run transcription to completion on the calling (worker) thread. The Groq
    /// arm drives its async call on a current-thread tokio runtime (KTD4); the
    /// local arm is already synchronous-blocking.
    pub fn transcribe_blocking(&self, audio: &[f32]) -> Result<String, TranscribeError> {
        match self {
            ActiveBackend::Local(b) => b.transcribe(audio),
            ActiveBackend::Groq(b) => {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| TranscribeError::Network(e.to_string()))?;
                rt.block_on(b.transcribe(audio))
            }
        }
    }
}

/// Result of a runtime backend switch.
#[derive(Debug, PartialEq, Eq)]
pub enum SwitchOutcome {
    Switched,
    Unchanged,
    /// The target backend is not available (e.g. local model never loaded). The
    /// previous backend stays active and the caller notifies the user (R14).
    Rejected,
}

/// Holds both backends (each built at most once) and the active selection.
pub struct Backends {
    groq: Option<ActiveBackend>,
    local: Option<ActiveBackend>,
    active: Backend,
}

impl Backends {
    /// Build available backends and choose the initial active one, falling back
    /// local→Groq when the local model can't load (R14).
    pub fn initialize(raw: &RawConfig) -> Result<(Backends, Option<String>), FatalError> {
        let groq = raw
            .groq_api_key
            .clone()
            .map(|key| ActiveBackend::Groq(GroqBackend::new(key)));

        let local = match raw.whisper_model_path.as_ref() {
            Some(path) => match LocalWhisperBackend::try_new(path) {
                Ok(b) => Some(ActiveBackend::Local(b)),
                Err(e) => {
                    tracing::warn!("local backend unavailable: {e}");
                    None
                }
            },
            None => None,
        };

        let (active, notice) = resolve_initial(raw.backend, groq.is_some(), local.is_some())?;
        Ok((
            Backends {
                groq,
                local,
                active,
            },
            notice.map(str::to_string),
        ))
    }

    pub fn active_kind(&self) -> Backend {
        self.active
    }

    /// A clone of the currently active backend, for dispatch to a worker thread.
    pub fn current(&self) -> ActiveBackend {
        match self.active {
            Backend::Groq => self.groq.clone().expect("active backend is present"),
            Backend::Local => self.local.clone().expect("active backend is present"),
        }
    }

    /// Attempt to switch the active backend (tray menu). Rejected if the target
    /// is unavailable (R14); the previous backend stays active.
    pub fn switch(&mut self, to: Backend) -> SwitchOutcome {
        let outcome = resolve_switch(self.active, to, self.groq.is_some(), self.local.is_some());
        if outcome == SwitchOutcome::Switched {
            self.active = to;
        }
        outcome
    }
}

/// Pure: choose the initial active backend + optional fallback notice.
fn resolve_initial(
    desired: Backend,
    groq_avail: bool,
    local_avail: bool,
) -> Result<(Backend, Option<&'static str>), FatalError> {
    match desired {
        Backend::Groq => {
            if groq_avail {
                Ok((Backend::Groq, None))
            } else {
                Err(FatalError::GroqSelectedNoKey)
            }
        }
        Backend::Local => {
            if local_avail {
                Ok((Backend::Local, None))
            } else if groq_avail {
                Ok((
                    Backend::Groq,
                    Some("Local model unavailable; falling back to Groq."),
                ))
            } else {
                Err(FatalError::LocalSelectedUnavailableNoFallback)
            }
        }
    }
}

/// Pure: decide a runtime switch given backend availability.
fn resolve_switch(
    active: Backend,
    to: Backend,
    groq_avail: bool,
    local_avail: bool,
) -> SwitchOutcome {
    if active == to {
        return SwitchOutcome::Unchanged;
    }
    let target_available = match to {
        Backend::Groq => groq_avail,
        Backend::Local => local_avail,
    };
    if target_available {
        SwitchOutcome::Switched
    } else {
        SwitchOutcome::Rejected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groq_selected_with_key_is_active() {
        let (active, notice) = resolve_initial(Backend::Groq, true, false).unwrap();
        assert_eq!(active, Backend::Groq);
        assert!(notice.is_none());
    }

    #[test]
    fn local_selected_with_model_is_active() {
        let (active, notice) = resolve_initial(Backend::Local, false, true).unwrap();
        assert_eq!(active, Backend::Local);
        assert!(notice.is_none());
    }

    #[test]
    fn local_unloadable_with_key_falls_back_to_groq() {
        // R14: startup fallback.
        let (active, notice) = resolve_initial(Backend::Local, true, false).unwrap();
        assert_eq!(active, Backend::Groq);
        assert!(notice.is_some());
    }

    #[test]
    fn local_unloadable_without_key_is_fatal() {
        // R14: no fallback available.
        let err = resolve_initial(Backend::Local, false, false).unwrap_err();
        assert!(matches!(
            err,
            FatalError::LocalSelectedUnavailableNoFallback
        ));
    }

    #[test]
    fn groq_selected_without_key_is_fatal() {
        let err = resolve_initial(Backend::Groq, false, false).unwrap_err();
        assert!(matches!(err, FatalError::GroqSelectedNoKey));
    }

    #[test]
    fn switch_to_available_backend_succeeds() {
        assert_eq!(
            resolve_switch(Backend::Groq, Backend::Local, true, true),
            SwitchOutcome::Switched
        );
    }

    #[test]
    fn switch_to_unloaded_local_is_rejected() {
        // R14: runtime switch to an unusable local backend is refused.
        assert_eq!(
            resolve_switch(Backend::Groq, Backend::Local, true, false),
            SwitchOutcome::Rejected
        );
    }

    #[test]
    fn switch_to_current_backend_is_unchanged() {
        assert_eq!(
            resolve_switch(Backend::Groq, Backend::Groq, true, false),
            SwitchOutcome::Unchanged
        );
    }
}
