//! Configuration loading (KTD9).
//!
//! Values come from three sources, resolved by precedence
//! **env vars → `%APPDATA%\xiuxiu\.env` → `<exe-dir>/.env`**. The current
//! working directory is never consulted (a tray app launched from a shortcut or
//! the Startup folder has an unpredictable cwd).
//!
//! Parsing/precedence ([`resolve`]) and strict validation
//! ([`RawConfig::validate`]) are pure functions over plain maps so they are
//! testable without touching the process environment. The richer
//! "fall back to Groq if local is unusable" decision (R14) lives in
//! [`crate::transcription::select_backend`], which consumes the [`RawConfig`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use thiserror::Error;

const KEY_BACKEND: &str = "BACKEND";
const KEY_GROQ: &str = "GROQ_API_KEY";
const KEY_MODEL: &str = "WHISPER_MODEL_PATH";
const KEY_HOTKEY: &str = "HOTKEY";
const KEY_TYPING_DELAY_MS: &str = "TYPING_DELAY_MS";

/// Default backend when `BACKEND` is unset. Groq is the lighter default (no
/// multi-hundred-MB model needed); validation/fallback guides the user if no
/// key is present.
const DEFAULT_BACKEND: Backend = Backend::Groq;

/// Default global hotkey when `HOTKEY` is unset. Less contended than
/// Ctrl+Shift+Space (which Windows IMEs / PowerToys commonly hold). Parsed by
/// `global-hotkey`'s accelerator syntax in `app.rs`. (AltGr caveat: on layouts
/// where Right-Alt is AltGr, AltGr synthesizes Ctrl+Alt — those users should
/// override `HOTKEY`.)
const DEFAULT_HOTKEY: &str = "Ctrl+Alt+Space";

/// Default per-character typing delay (ms) when `TYPING_DELAY_MS` is unset.
/// Throttles synthetic keystrokes so target apps don't drop characters.
const DEFAULT_TYPING_DELAY_MS: u64 = 8;

/// Which transcription backend is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Local,
    Groq,
}

impl FromStr for Backend {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "local" => Ok(Backend::Local),
            "groq" => Ok(Backend::Groq),
            other => Err(ConfigError::UnknownBackend(other.to_string())),
        }
    }
}

/// A credential that never reveals itself through `Debug`/`Display` (R16).
/// The only way to read the value is [`Secret::expose`], used solely when
/// building the `Authorization` header in `groq.rs`.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// Reveal the secret. Call sites are deliberately rare and audited.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("***")
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("unknown BACKEND value '{0}' (expected 'local' or 'groq')")]
    UnknownBackend(String),
    #[error("BACKEND=groq requires GROQ_API_KEY to be set")]
    MissingGroqKey,
    #[error("BACKEND=local requires WHISPER_MODEL_PATH to be set")]
    MissingModelPath,
    #[error("WHISPER_MODEL_PATH does not point to an existing file: {0}")]
    ModelFileMissing(PathBuf),
}

/// Parsed configuration before backend-selection/fallback is applied.
///
/// `Debug` is derived but safe: `groq_api_key` is a [`Secret`], whose own
/// `Debug` prints `Secret(***)`, so the key cannot leak through a derived
/// `Debug` of this struct (R16).
#[derive(Debug, Clone)]
pub struct RawConfig {
    pub backend: Backend,
    pub groq_api_key: Option<Secret>,
    pub whisper_model_path: Option<PathBuf>,
    /// Global hotkey as an accelerator string (e.g. `Ctrl+Alt+Space`). Kept as a
    /// raw string here; parsed into a `HotKey` in `app.rs` (KTD1).
    pub hotkey: String,
    /// Per-character typing delay in milliseconds (injection throttle). The
    /// `Duration` conversion happens at the injection call site in `app.rs`.
    pub typing_delay_ms: u64,
}

impl RawConfig {
    /// Strict per-backend validation (R11). The transcription layer uses the
    /// raw values directly for the R14 fallback path instead of this gate, so
    /// `validate` is primarily the standalone correctness check exercised by
    /// tests and by simple callers that want a hard error.
    pub fn validate(&self) -> Result<(), ConfigError> {
        match self.backend {
            Backend::Groq => {
                if self.groq_api_key.is_none() {
                    return Err(ConfigError::MissingGroqKey);
                }
            }
            Backend::Local => match &self.whisper_model_path {
                None => return Err(ConfigError::MissingModelPath),
                Some(path) if !path.is_file() => {
                    return Err(ConfigError::ModelFileMissing(path.clone()));
                }
                Some(_) => {}
            },
        }
        Ok(())
    }
}

/// Resolve a [`RawConfig`] from three sources by precedence (env > appdata >
/// exedir). Pure over the supplied maps; the only failure is an unparseable
/// `BACKEND` value. Empty/whitespace values are treated as absent.
pub fn resolve(
    env: &HashMap<String, String>,
    appdata: &HashMap<String, String>,
    exedir: &HashMap<String, String>,
) -> Result<RawConfig, ConfigError> {
    let pick = |key: &str| -> Option<String> {
        env.get(key)
            .or_else(|| appdata.get(key))
            .or_else(|| exedir.get(key))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    let backend = match pick(KEY_BACKEND) {
        Some(v) => v.parse::<Backend>()?,
        None => DEFAULT_BACKEND,
    };

    Ok(RawConfig {
        backend,
        groq_api_key: pick(KEY_GROQ).map(Secret::new),
        whisper_model_path: pick(KEY_MODEL).map(PathBuf::from),
        hotkey: pick(KEY_HOTKEY).unwrap_or_else(|| DEFAULT_HOTKEY.to_string()),
        typing_delay_ms: pick(KEY_TYPING_DELAY_MS)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_TYPING_DELAY_MS),
    })
}

/// Parse a `.env` file into a map without mutating the process environment.
/// A missing/unreadable file yields an empty map (it is simply absent).
pub fn load_env_file(path: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(iter) = dotenvy::from_path_iter(path) {
        for (key, value) in iter.flatten() {
            map.insert(key, value);
        }
    }
    map
}

/// Read the known keys from the process environment. `env_map` only forwards
/// the keys it lists, so every config key must appear here or it is invisible
/// as a real environment variable (it would only work via a `.env` file).
fn env_map() -> HashMap<String, String> {
    [
        KEY_BACKEND,
        KEY_GROQ,
        KEY_MODEL,
        KEY_HOTKEY,
        KEY_TYPING_DELAY_MS,
    ]
    .iter()
    .filter_map(|key| std::env::var(key).ok().map(|v| (key.to_string(), v)))
    .collect()
}

/// Resolve config from the live environment + `.env` files at the two
/// supported locations. Used at startup; the integration test exercises the
/// file-reading path via [`load_env_file`] + [`resolve`] against a temp dir.
pub fn load() -> Result<RawConfig, ConfigError> {
    let env = env_map();

    let appdata = std::env::var("APPDATA")
        .ok()
        .map(|base| load_env_file(&Path::new(&base).join("xiuxiu").join(".env")))
        .unwrap_or_default();

    let exedir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| load_env_file(&dir.join(".env"))))
        .unwrap_or_default();

    resolve(&env, &appdata, &exedir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn empty() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn reads_all_keys_from_a_single_source() {
        let exedir = map(&[
            ("BACKEND", "groq"),
            ("GROQ_API_KEY", "gsk_test"),
            ("WHISPER_MODEL_PATH", "/models/ggml-base.en.bin"),
        ]);
        let cfg = resolve(&empty(), &empty(), &exedir).unwrap();
        assert_eq!(cfg.backend, Backend::Groq);
        assert_eq!(cfg.groq_api_key.as_ref().unwrap().expose(), "gsk_test");
        assert_eq!(
            cfg.whisper_model_path.as_deref(),
            Some(Path::new("/models/ggml-base.en.bin"))
        );
    }

    #[test]
    fn env_overrides_lower_sources() {
        let env = map(&[("GROQ_API_KEY", "from_env")]);
        let exedir = map(&[("BACKEND", "groq"), ("GROQ_API_KEY", "from_file")]);
        let cfg = resolve(&env, &empty(), &exedir).unwrap();
        assert_eq!(cfg.groq_api_key.unwrap().expose(), "from_env");
    }

    #[test]
    fn appdata_wins_over_exedir_but_env_wins_over_both() {
        let env = map(&[("BACKEND", "local")]);
        let appdata = map(&[("BACKEND", "groq"), ("GROQ_API_KEY", "appdata_key")]);
        let exedir = map(&[("GROQ_API_KEY", "exedir_key")]);
        let cfg = resolve(&env, &appdata, &exedir).unwrap();
        assert_eq!(cfg.backend, Backend::Local); // env wins
        assert_eq!(cfg.groq_api_key.unwrap().expose(), "appdata_key"); // appdata > exedir
    }

    #[test]
    fn groq_without_key_fails_validation() {
        let cfg = resolve(&empty(), &empty(), &map(&[("BACKEND", "groq")])).unwrap();
        assert!(matches!(cfg.validate(), Err(ConfigError::MissingGroqKey)));
    }

    #[test]
    fn local_with_nonexistent_model_fails_validation() {
        let exedir = map(&[
            ("BACKEND", "local"),
            ("WHISPER_MODEL_PATH", "/definitely/not/here.bin"),
        ]);
        let cfg = resolve(&empty(), &empty(), &exedir).unwrap();
        assert!(matches!(
            cfg.validate(),
            Err(ConfigError::ModelFileMissing(_))
        ));
    }

    #[test]
    fn local_without_model_path_fails_validation() {
        let cfg = resolve(&empty(), &empty(), &map(&[("BACKEND", "local")])).unwrap();
        assert!(matches!(cfg.validate(), Err(ConfigError::MissingModelPath)));
    }

    #[test]
    fn unset_backend_defaults_to_groq() {
        let cfg = resolve(&empty(), &empty(), &map(&[("GROQ_API_KEY", "k")])).unwrap();
        assert_eq!(cfg.backend, Backend::Groq);
    }

    #[test]
    fn unknown_backend_value_errors() {
        let err = resolve(&empty(), &empty(), &map(&[("BACKEND", "azure")])).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownBackend(v) if v == "azure"));
    }

    #[test]
    fn empty_values_are_treated_as_absent() {
        let env = map(&[("GROQ_API_KEY", "   ")]);
        let cfg = resolve(&env, &empty(), &map(&[("BACKEND", "groq")])).unwrap();
        assert!(cfg.groq_api_key.is_none());
        assert!(matches!(cfg.validate(), Err(ConfigError::MissingGroqKey)));
    }

    #[test]
    fn secret_never_leaks_through_debug() {
        let cfg = resolve(&empty(), &empty(), &map(&[("GROQ_API_KEY", "supersecret")])).unwrap();
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("supersecret"));
        assert!(rendered.contains("***"));
    }

    #[test]
    fn unset_hotkey_uses_default() {
        let cfg = resolve(&empty(), &empty(), &map(&[("GROQ_API_KEY", "k")])).unwrap();
        assert_eq!(cfg.hotkey, "Ctrl+Alt+Space");
    }

    #[test]
    fn hotkey_override_is_read() {
        let cfg = resolve(
            &empty(),
            &empty(),
            &map(&[("GROQ_API_KEY", "k"), ("HOTKEY", "Alt+Space")]),
        )
        .unwrap();
        assert_eq!(cfg.hotkey, "Alt+Space");
    }

    #[test]
    fn hotkey_precedence_env_over_appdata_over_exedir() {
        let env = map(&[("HOTKEY", "Ctrl+Alt+J")]);
        let appdata = map(&[("HOTKEY", "Ctrl+Alt+K")]);
        let exedir = map(&[("GROQ_API_KEY", "k"), ("HOTKEY", "Ctrl+Alt+L")]);
        assert_eq!(
            resolve(&env, &appdata, &exedir).unwrap().hotkey,
            "Ctrl+Alt+J"
        );
        assert_eq!(
            resolve(&empty(), &appdata, &exedir).unwrap().hotkey,
            "Ctrl+Alt+K"
        );
        assert_eq!(
            resolve(&empty(), &empty(), &exedir).unwrap().hotkey,
            "Ctrl+Alt+L"
        );
    }

    #[test]
    fn empty_hotkey_falls_back_to_default() {
        let cfg = resolve(
            &map(&[("HOTKEY", "   ")]),
            &empty(),
            &map(&[("GROQ_API_KEY", "k")]),
        )
        .unwrap();
        assert_eq!(cfg.hotkey, "Ctrl+Alt+Space");
    }

    #[test]
    fn unset_typing_delay_uses_default() {
        let cfg = resolve(&empty(), &empty(), &map(&[("GROQ_API_KEY", "k")])).unwrap();
        assert_eq!(cfg.typing_delay_ms, 8);
    }

    #[test]
    fn typing_delay_override_is_read() {
        let cfg = resolve(
            &empty(),
            &empty(),
            &map(&[("GROQ_API_KEY", "k"), ("TYPING_DELAY_MS", "20")]),
        )
        .unwrap();
        assert_eq!(cfg.typing_delay_ms, 20);
    }

    #[test]
    fn typing_delay_precedence_env_over_exedir() {
        let env = map(&[("TYPING_DELAY_MS", "5")]);
        let exedir = map(&[("GROQ_API_KEY", "k"), ("TYPING_DELAY_MS", "50")]);
        assert_eq!(resolve(&env, &empty(), &exedir).unwrap().typing_delay_ms, 5);
        assert_eq!(
            resolve(&empty(), &empty(), &exedir)
                .unwrap()
                .typing_delay_ms,
            50
        );
    }

    #[test]
    fn non_numeric_typing_delay_falls_back_to_default() {
        let cfg = resolve(
            &map(&[("TYPING_DELAY_MS", "fast")]),
            &empty(),
            &map(&[("GROQ_API_KEY", "k")]),
        )
        .unwrap();
        assert_eq!(cfg.typing_delay_ms, 8);
    }
}
