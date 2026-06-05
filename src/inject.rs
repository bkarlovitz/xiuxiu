//! Unicode text injection into the focused window via `enigo` (R2).
//!
//! Two safety measures:
//!   - [`sanitize`] strips NUL (which enigo forbids) **and** other control
//!     characters below `0x20` (CR/LF/ESC/TAB). A transcript injected into a
//!     focused terminal must not be able to emit newlines or escape sequences
//!     that execute commands (S3 trust boundary).
//!   - text is typed one character at a time with a short delay between
//!     characters so a long string does not outrun slow targets
//!     (terminals/Electron/RDP). enigo's `text()` sends the whole string in one
//!     `SendInput` burst with no inter-key delay, which drops characters on
//!     targets that can't keep up; pacing it ourselves is the fix (enigo 0.6 has
//!     no Windows delay setting). Injection runs on the transcription worker
//!     thread, never the UI thread (KTD5), so the sleeps don't freeze the loop.
//!
//! `Enigo` is constructed once per [`inject`] call.

use std::time::Duration;

use enigo::{Enigo, Keyboard, Settings};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InjectError {
    #[error("failed to initialize the input backend: {0}")]
    Init(String),
    #[error("failed to type text: {0}")]
    Type(String),
}

/// Strip control characters so injected text cannot execute in a terminal and
/// cannot contain the NUL byte enigo rejects. Normal Unicode (including emoji)
/// is preserved. This is the sequence [`inject`] will type, in order.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|&c| (c as u32) >= 0x20 && c != '\u{7f}')
        .collect()
}

/// Sanitize `text` and type it into the focused window one character at a time,
/// sleeping `delay` between characters so the target app keeps up. A string that
/// sanitizes to empty types nothing.
///
/// Uses `text()` per char (the pure `KEYEVENTF_UNICODE` path) rather than
/// `key(Key::Unicode, _)` so emoji / non-BMP characters are handled correctly.
pub fn inject(text: &str, delay: Duration) -> Result<(), InjectError> {
    let cleaned = sanitize(text);
    if cleaned.is_empty() {
        return Ok(());
    }

    let mut enigo =
        Enigo::new(&Settings::default()).map_err(|e| InjectError::Init(e.to_string()))?;

    let mut chars = cleaned.chars().peekable();
    while let Some(c) = chars.next() {
        enigo
            .text(&c.to_string())
            .map_err(|e| InjectError::Type(e.to_string()))?;
        if chars.peek().is_some() {
            std::thread::sleep(delay);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_nul_and_control_keeps_unicode() {
        let input = "hello\0 world\u{1b}[31m ❤️\ttab";
        let out = sanitize(input);
        assert!(!out.contains('\0'));
        assert!(!out.contains('\u{1b}'));
        assert!(!out.contains('\t'));
        assert!(out.contains("hello"));
        assert!(out.contains("world"));
        assert!(out.contains('❤'));
    }

    #[test]
    fn sanitize_neutralizes_embedded_newline_and_escape() {
        // A transcript that, unsanitized, would submit a shell command.
        let dangerous = "rm -rf /\nyes\r\n\u{1b}]0;pwned\u{7}";
        let out = sanitize(dangerous);
        assert!(!out.contains('\n'));
        assert!(!out.contains('\r'));
        assert!(!out.contains('\u{1b}'));
        // The literal words survive; only the control chars are gone.
        assert!(out.contains("rm -rf /"));
    }

    #[test]
    fn sanitized_text_round_trips_char_by_char() {
        // `inject` types `sanitize(text).chars()` in order; the per-char sequence
        // must reconstruct the sanitized string with no loss or reorder.
        let input = "Tomorrow morning ❤️ 🚀 done.";
        let sanitized = sanitize(input);
        let typed: String = sanitized.chars().collect();
        assert_eq!(typed, sanitized);
    }

    #[test]
    fn all_control_chars_sanitize_to_empty() {
        // An all-control input sanitizes to empty → inject types nothing.
        assert!(sanitize("\0\n\r\u{1b}").is_empty());
    }
}
