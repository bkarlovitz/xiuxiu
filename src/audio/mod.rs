//! Audio capture (dedicated thread, build-on-press) and preprocessing.

pub mod capture;
pub mod preprocess;

pub use capture::{AudioError, AudioHandle, CapturedAudio};
pub use preprocess::{passes_gate, to_canonical};
