//! Xiuxiu — push-to-talk voice dictation tray app for Windows.
//!
//! All logic lives in the library so it is unit/integration testable; the
//! binary (`src/main.rs`) is a thin shell that sets the Windows subsystem and
//! calls [`run`].

pub mod app;
pub mod audio;
pub mod config;
pub mod inject;
pub mod logging;
pub mod transcription;
pub mod tray;

pub use app::run;
