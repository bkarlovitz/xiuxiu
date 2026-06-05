//! Dedicated audio thread that owns the `cpal` stream (KTD12/KTD13).
//!
//! The `cpal::Stream` is `!Send` and `build`/`drop` make blocking, COM-sensitive
//! WASAPI calls, so it never crosses onto the event-loop thread. The main thread
//! talks to this thread only via:
//!   - a command channel (`Start` / `Stop` / `Shutdown`),
//!   - a one-shot init-result channel (mic probe → R13),
//!   - and a caller-provided sink that receives the finalized [`CapturedAudio`]
//!     (the app wires this to `proxy.send_event(UserEvent::AudioCaptured(..))`,
//!     so this module has no dependency on the event enum).
//!
//! Capture is **build-on-press** (KTD13): the stream is built on `Start` and
//! dropped on `Stop`, so the microphone is open only while the hotkey is held.

use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use thiserror::Error;

use super::preprocess::{i16_to_f32, u16_to_f32};

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("no default input device (microphone) available")]
    NoInputDevice,
    #[error("could not query input device config: {0}")]
    Config(String),
    #[error("could not build input stream: {0}")]
    BuildStream(String),
}

/// A finalized capture, in the device's native interleaved f32 format. Downmix
/// and resample to the canonical 16 kHz mono happen later, on the worker.
#[derive(Debug, Clone)]
pub struct CapturedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

enum AudioCommand {
    Start,
    Stop,
    Shutdown,
}

/// Accumulates samples for one recording session. Factored out of the cpal
/// callback so the accumulation logic is unit-testable.
#[derive(Default)]
struct SessionBuffer {
    samples: Vec<f32>,
}

impl SessionBuffer {
    fn start(&mut self) {
        self.samples.clear();
    }
    fn push_f32(&mut self, data: &[f32]) {
        self.samples.extend_from_slice(data);
    }
    fn push_i16(&mut self, data: &[i16]) {
        self.samples.extend(data.iter().map(|&s| i16_to_f32(s)));
    }
    fn push_u16(&mut self, data: &[u16]) {
        self.samples.extend(data.iter().map(|&s| u16_to_f32(s)));
    }
    fn take(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.samples)
    }
}

/// Handle to the audio thread. `Send` (holds only channels + a join handle); the
/// `!Send` stream stays pinned inside the thread.
pub struct AudioHandle {
    cmd_tx: Sender<AudioCommand>,
    join: Option<JoinHandle<()>>,
}

impl AudioHandle {
    /// Spawn the audio thread and run the startup mic probe. The returned
    /// `Result` is the probe outcome (R13): `Err` means no usable microphone.
    pub fn spawn(sink: Box<dyn Fn(CapturedAudio) + Send>) -> (AudioHandle, Result<(), AudioError>) {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::sync_channel(1);

        let join = thread::Builder::new()
            .name("xiuxiu-audio".to_string())
            .spawn(move || audio_thread(cmd_rx, init_tx, sink))
            .expect("spawn audio thread");

        // Block briefly until the probe completes; a dead channel means the
        // thread failed before probing.
        let init = init_rx.recv().unwrap_or(Err(AudioError::NoInputDevice));

        (
            AudioHandle {
                cmd_tx,
                join: Some(join),
            },
            init,
        )
    }

    /// Begin recording (hotkey press). Builds the stream on the audio thread.
    pub fn start(&self) {
        let _ = self.cmd_tx.send(AudioCommand::Start);
    }

    /// Stop recording (hotkey release). Drops the stream and delivers the buffer
    /// through the sink.
    pub fn stop(&self) {
        let _ = self.cmd_tx.send(AudioCommand::Stop);
    }
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(AudioCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn audio_thread(
    cmd_rx: Receiver<AudioCommand>,
    init_tx: SyncSender<Result<(), AudioError>>,
    sink: Box<dyn Fn(CapturedAudio) + Send>,
) {
    // Startup probe (R13): validate a default input device exists and its config
    // can be queried, without holding the mic open (privacy — KTD13).
    let _ = init_tx.send(probe_device());

    // Active capture state lives only between Start and Stop.
    let mut active: Option<(cpal::Stream, Arc<Mutex<SessionBuffer>>, u32, u16)> = None;

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            AudioCommand::Start => {
                if active.is_some() {
                    continue; // already recording; ignore (defense in depth)
                }
                match start_capture() {
                    Ok((stream, buffer, rate, channels)) => {
                        if let Err(e) = stream.play() {
                            tracing::error!("failed to start audio stream: {e}");
                            continue;
                        }
                        active = Some((stream, buffer, rate, channels));
                    }
                    Err(e) => tracing::error!("failed to build audio stream: {e}"),
                }
            }
            AudioCommand::Stop => {
                if let Some((stream, buffer, rate, channels)) = active.take() {
                    drop(stream); // stop + flush the final callback
                    let samples = buffer.lock().map(|mut b| b.take()).unwrap_or_default();
                    sink(CapturedAudio {
                        samples,
                        sample_rate: rate,
                        channels,
                    });
                }
            }
            AudioCommand::Shutdown => break,
        }
    }
}

fn probe_device() -> Result<(), AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or(AudioError::NoInputDevice)?;
    device
        .default_input_config()
        .map_err(|e| AudioError::Config(e.to_string()))?;
    Ok(())
}

fn start_capture() -> Result<(cpal::Stream, Arc<Mutex<SessionBuffer>>, u32, u16), AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or(AudioError::NoInputDevice)?;
    let supported = device
        .default_input_config()
        .map_err(|e| AudioError::Config(e.to_string()))?;

    let rate = supported.sample_rate().0;
    let channels = supported.channels();
    let buffer = Arc::new(Mutex::new(SessionBuffer::default()));
    {
        // Fresh session (clear→set edge).
        if let Ok(mut b) = buffer.lock() {
            b.start();
        }
    }
    let stream = build_stream(&device, &supported, buffer.clone())?;
    Ok((stream, buffer, rate, channels))
}

fn stream_err(err: cpal::StreamError) {
    tracing::error!("cpal stream error: {err}");
}

fn build_stream(
    device: &cpal::Device,
    supported: &cpal::SupportedStreamConfig,
    buffer: Arc<Mutex<SessionBuffer>>,
) -> Result<cpal::Stream, AudioError> {
    let config: cpal::StreamConfig = supported.config();

    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => {
            let buf = buffer.clone();
            device.build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if let Ok(mut b) = buf.lock() {
                        b.push_f32(data);
                    }
                },
                stream_err,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let buf = buffer.clone();
            device.build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    if let Ok(mut b) = buf.lock() {
                        b.push_i16(data);
                    }
                },
                stream_err,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let buf = buffer.clone();
            device.build_input_stream(
                &config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    if let Ok(mut b) = buf.lock() {
                        b.push_u16(data);
                    }
                },
                stream_err,
                None,
            )
        }
        other => {
            return Err(AudioError::Config(format!(
                "unsupported sample format: {other:?}"
            )))
        }
    }
    .map_err(|e| AudioError::BuildStream(e.to_string()))?;

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_buffer_accumulates_between_start_and_take() {
        let mut buf = SessionBuffer::default();
        buf.start();
        buf.push_f32(&[0.1, 0.2]);
        buf.push_f32(&[0.3]);
        let taken = buf.take();
        assert_eq!(taken, vec![0.1, 0.2, 0.3]);
        // take resets the buffer
        assert!(buf.take().is_empty());
    }

    #[test]
    fn session_buffer_start_clears_previous_session() {
        let mut buf = SessionBuffer::default();
        buf.push_f32(&[9.9]);
        buf.start(); // clear→set edge discards the stale partial session
        buf.push_f32(&[0.5]);
        assert_eq!(buf.take(), vec![0.5]);
    }

    #[test]
    fn session_buffer_converts_integer_samples() {
        let mut buf = SessionBuffer::default();
        buf.push_i16(&[i16::MAX, i16::MIN]);
        let out = buf.take();
        assert!((out[0] - 0.999_97).abs() < 1e-3);
        assert!((out[1] + 1.0).abs() < 1e-6);
    }
}
