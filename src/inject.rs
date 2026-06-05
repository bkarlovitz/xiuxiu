//! Unicode text injection into the focused window via `enigo` (R2).
//!
//! Two safety measures:
//!   - [`sanitize`] strips NUL (which enigo forbids) **and** other control
//!     characters below `0x20` (CR/LF/ESC/TAB). A transcript injected into a
//!     focused terminal must not be able to emit newlines or escape sequences
//!     that execute commands (S3 trust boundary).
//!   - text is typed in small chunks with a short inter-chunk delay so a long
//!     string does not outrun slow targets (terminals/Electron/RDP).
//!
//! `Enigo` is constructed once per [`inject`] call (not per chunk) to avoid
//! repeating the Windows `SendInput`/COM setup cost.

use std::time::Duration;

use enigo::{Enigo, Keyboard, Settings};
use thiserror::Error;

/// Max characters typed per chunk.
const CHUNK_CHARS: usize = 64;
/// Pause between chunks so slow targets keep up.
const INTER_CHUNK_DELAY: Duration = Duration::from_millis(10);

#[derive(Debug, Error)]
pub enum InjectError {
    #[error("failed to initialize the input backend: {0}")]
    Init(String),
    #[error("failed to type text: {0}")]
    Type(String),
}

/// Strip control characters so injected text cannot execute in a terminal and
/// cannot contain the NUL byte enigo rejects. Normal Unicode (including emoji)
/// is preserved.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|&c| (c as u32) >= 0x20 && c != '\u{7f}')
        .collect()
}

/// Split text into chunks of at most `max_chars` characters (char-aligned so
/// multibyte characters are never split). Empty input yields no chunks.
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(max_chars.max(1))
        .map(|c| c.iter().collect())
        .collect()
}

/// Sanitize and type `text` into the focused window. A string that sanitizes to
/// empty results in no injection call.
pub fn inject(text: &str) -> Result<(), InjectError> {
    let cleaned = sanitize(text);
    if cleaned.is_empty() {
        return Ok(());
    }

    let mut enigo =
        Enigo::new(&Settings::default()).map_err(|e| InjectError::Init(e.to_string()))?;

    let chunks = chunk_text(&cleaned, CHUNK_CHARS);
    let last = chunks.len().saturating_sub(1);
    for (i, chunk) in chunks.iter().enumerate() {
        enigo
            .text(chunk)
            .map_err(|e| InjectError::Type(e.to_string()))?;
        if i != last {
            std::thread::sleep(INTER_CHUNK_DELAY);
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
    fn chunk_splits_preserving_content_and_order() {
        let text: String = "abcdefghij".repeat(20); // 200 chars
        let chunks = chunk_text(&text, CHUNK_CHARS);
        assert!(chunks.iter().all(|c| c.chars().count() <= CHUNK_CHARS));
        assert_eq!(chunks.concat(), text); // nothing lost or reordered
        assert_eq!(chunks.len(), (200 + CHUNK_CHARS - 1) / CHUNK_CHARS);
    }

    #[test]
    fn chunk_short_string_is_single_chunk() {
        assert_eq!(chunk_text("hi", CHUNK_CHARS), vec!["hi".to_string()]);
    }

    #[test]
    fn chunk_empty_string_yields_no_chunks() {
        assert!(chunk_text("", CHUNK_CHARS).is_empty());
        // A string that is all control chars sanitizes to empty → no chunks.
        assert!(chunk_text(&sanitize("\0\n\r"), CHUNK_CHARS).is_empty());
    }

    #[test]
    fn chunk_does_not_split_multibyte_characters() {
        let emoji = "❤️🎉🚀".repeat(50);
        let chunks = chunk_text(&emoji, 3);
        assert_eq!(chunks.concat(), emoji);
    }
}
