//! File-based logging and a panic hook that survives `windows_subsystem =
//! "windows"` (KTD8).
//!
//! With the console suppressed, stdout/stderr — including panic messages — go
//! nowhere. So logs are written to `xiuxiu.log` next to the executable, and a
//! panic hook writes the panic + backtrace to that file *and* pops a message
//! box so a crash is never silent.
//!
//! **Hygiene (R16):** transcribed text and the API key are never logged. Call
//! sites that want to log something about a transcript use
//! [`describe_transcript`], which records length only, never content.

use std::backtrace::Backtrace;
use std::panic;
use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;

/// Directory for the log file: next to the executable, falling back to the cwd
/// if the exe path can't be resolved.
pub fn log_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Initialize file logging. The returned [`WorkerGuard`] must be held for the
/// program's lifetime — dropping it flushes and stops the background writer.
///
/// Note: a current-user-only ACL on the log file is a follow-up (it requires
/// Win32 `SetNamedSecurityInfo`); for now the README directs users to install
/// under a user-profile/`%APPDATA%` location so the directory is not
/// world-readable. The log shares a directory with `.env`, so this matters.
pub fn init() -> WorkerGuard {
    let dir = log_dir();
    let file_appender = tracing_appender::rolling::never(dir, "xiuxiu.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    guard
}

/// Install a panic hook that logs the panic + backtrace and shows a message box.
pub fn install_panic_hook() {
    panic::set_hook(Box::new(|info| {
        let backtrace = Backtrace::force_capture();
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown location".to_string());
        let message = payload_str(info);

        tracing::error!(%location, "panic: {message}\n{backtrace}");
        show_error_dialog(
            "Xiuxiu crashed",
            &format!("{message}\nat {location}\n\nSee xiuxiu.log next to the executable."),
        );
    }));
}

fn payload_str(info: &panic::PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "Box<dyn Any>".to_string()
    }
}

/// Show a blocking, native error dialog. On non-Windows this only logs (the
/// dialog is a Windows-no-console affordance). Safe to call before/without a
/// tray.
pub fn show_error_dialog(title: &str, body: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let wide_title = to_wide(title);
        let wide_body = to_wide(body);
        // SAFETY: both pointers are valid, NUL-terminated UTF-16 buffers that
        // outlive the call; a null HWND shows an unowned modal dialog.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                wide_body.as_ptr(),
                wide_title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    {
        tracing::error!("{title}: {body}");
    }
}

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Describe a transcript for logging **without** revealing its content (R16).
/// Use this anywhere you would otherwise be tempted to log the transcribed text.
pub fn describe_transcript(text: &str) -> String {
    format!("transcript: {} chars", text.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_transcript_reports_length_not_content(/* R16 */) {
        let secret_speech = "transfer all the money to account 12345";
        let described = describe_transcript(secret_speech);
        assert!(!described.contains("money"));
        assert!(!described.contains("12345"));
        assert!(described.contains("chars"));
        // length is the character count, not byte count
        assert!(described.contains(&secret_speech.chars().count().to_string()));
    }
}
