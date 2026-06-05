//! Event-loop coordination, recording state machine, and the `run` entry point
//! (KTD3/KTD4/KTD7).
//!
//! One winit event loop on the main thread funnels every source — tray, menu,
//! global hotkey, finalized capture, and worker results — through a single
//! [`UserEvent`] handled in [`App::user_event`]. The recording state machine
//! ([`Machine`]) is pure and unit-tested independently of winit.

use anyhow::Context;
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::WindowId;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use tray_icon::menu::MenuEvent;
use tray_icon::TrayIconEvent;

use crate::audio::{self, AudioHandle, CapturedAudio};
use crate::config::{self, Backend};
use crate::inject;
use crate::logging;
use crate::transcription::{Backends, SwitchOutcome};
use crate::tray::Tray;

// ---------------------------------------------------------------------------
// Pure state machine (KTD7) — testable without winit.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Idle,
    Recording,
    Finalizing,
    Transcribing,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PressAction {
    StartCapture,
    Ignore,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReleaseAction {
    StopCapture,
    Ignore,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CapturedAction {
    Dispatch(u64),
    DropClip,
    Ignore,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResultAction {
    /// The result belongs to the current recording — act on it.
    Current,
    /// A stale result from an abandoned recording — drop it (KTD7 backstop).
    Stale,
    Ignore,
}

pub struct Machine {
    state: AppState,
    gen_id: u64,
}

impl Machine {
    pub fn new() -> Self {
        Machine {
            state: AppState::Idle,
            gen_id: 0,
        }
    }

    pub fn state(&self) -> AppState {
        self.state
    }

    pub fn can_switch(&self) -> bool {
        self.state == AppState::Idle
    }

    pub fn on_press(&mut self) -> PressAction {
        if self.state == AppState::Idle {
            self.state = AppState::Recording;
            PressAction::StartCapture
        } else {
            PressAction::Ignore // single in-flight (R5)
        }
    }

    pub fn on_release(&mut self) -> ReleaseAction {
        if self.state == AppState::Recording {
            self.state = AppState::Finalizing;
            ReleaseAction::StopCapture
        } else {
            ReleaseAction::Ignore
        }
    }

    pub fn on_captured(&mut self, passes_gate: bool) -> CapturedAction {
        if self.state != AppState::Finalizing {
            return CapturedAction::Ignore; // spurious AudioCaptured
        }
        if passes_gate {
            self.gen_id += 1;
            self.state = AppState::Transcribing;
            CapturedAction::Dispatch(self.gen_id)
        } else {
            self.state = AppState::Idle;
            CapturedAction::DropClip // R4
        }
    }

    pub fn on_transcribed(&mut self, id: u64) -> ResultAction {
        if self.state != AppState::Transcribing {
            return ResultAction::Ignore;
        }
        self.state = AppState::Idle;
        if id == self.gen_id {
            ResultAction::Current
        } else {
            ResultAction::Stale
        }
    }
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Event plumbing
// ---------------------------------------------------------------------------

pub enum UserEvent {
    TrayIcon(TrayIconEvent),
    Menu(MenuEvent),
    Hotkey(GlobalHotKeyEvent),
    AudioCaptured(CapturedAudio),
    Transcribed {
        id: u64,
        result: Result<String, String>,
    },
}

struct App {
    machine: Machine,
    tray: Option<Tray>,
    backends: Backends,
    audio: AudioHandle,
    // Held for the process lifetime — dropping it unregisters the hotkey.
    _hotkey_manager: GlobalHotKeyManager,
    hotkey_id: u32,
    proxy: EventLoopProxy<UserEvent>,
    initial_active: Backend,
    /// Notices to show once the tray exists (startup mic error, fallback).
    pending_notices: Vec<(String, String)>,
}

impl App {
    fn on_hotkey(&mut self, event: GlobalHotKeyEvent) {
        if event.id != self.hotkey_id {
            return;
        }
        match event.state {
            HotKeyState::Pressed => {
                if matches!(self.machine.on_press(), PressAction::StartCapture) {
                    self.audio.start();
                    self.set_recording(true);
                }
            }
            HotKeyState::Released => {
                if matches!(self.machine.on_release(), ReleaseAction::StopCapture) {
                    self.audio.stop();
                    // Mic is closed at release (build-on-press, KTD13) — back to idle icon.
                    self.set_recording(false);
                }
            }
        }
    }

    fn on_menu(&mut self, event: MenuEvent, event_loop: &ActiveEventLoop) {
        let Some(tray) = self.tray.as_ref() else {
            return;
        };
        if tray.is_quit(&event.id) {
            event_loop.exit();
            return;
        }
        let Some(target) = tray.backend_for(&event.id) else {
            return;
        };
        if !self.machine.can_switch() {
            return; // switch only while idle (R7)
        }
        match self.backends.switch(target) {
            SwitchOutcome::Switched => {
                tray.set_active(self.backends.active_kind());
                tracing::info!("switched backend to {target:?}");
            }
            SwitchOutcome::Unchanged => {}
            SwitchOutcome::Rejected => {
                // R14: target unavailable — keep current, notify, leave checkmark.
                tray.set_active(self.backends.active_kind());
                tray.notify(
                    "Backend unavailable",
                    &format!(
                        "The {target:?} backend is not available (model not loaded). \
                         Staying on the current backend."
                    ),
                );
            }
        }
    }

    fn on_audio_captured(&mut self, captured: CapturedAudio) {
        let gate = audio::passes_gate(&captured.samples, captured.sample_rate, captured.channels);
        match self.machine.on_captured(gate) {
            CapturedAction::Dispatch(id) => self.dispatch(captured, id),
            CapturedAction::DropClip => {
                tracing::info!("dropped clip (too short or silent)");
            }
            CapturedAction::Ignore => {}
        }
    }

    fn dispatch(&self, captured: CapturedAudio, id: u64) {
        let backend = self.backends.current();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            // Preprocess + transcribe off the UI thread (KTD12/KTD4).
            let canonical =
                audio::to_canonical(&captured.samples, captured.sample_rate, captured.channels);
            let result = backend
                .transcribe_blocking(&canonical)
                .map_err(|e| e.to_string());
            let _ = proxy.send_event(UserEvent::Transcribed { id, result });
        });
    }

    fn on_transcribed(&mut self, id: u64, result: Result<String, String>) {
        match self.machine.on_transcribed(id) {
            ResultAction::Current => match result {
                Ok(text) => {
                    // R16: log length, never content.
                    tracing::info!("{}", logging::describe_transcript(&text));
                    if let Err(e) = inject::inject(&text) {
                        tracing::error!("text injection failed: {e}");
                    }
                }
                Err(message) => {
                    // R12: failed call is non-fatal — log + transient notice, keep running.
                    tracing::warn!("transcription failed: {message}");
                    if let Some(tray) = self.tray.as_ref() {
                        tray.transient("transcription failed");
                    }
                }
            },
            ResultAction::Stale => {
                tracing::info!("dropped stale transcription result (id {id})");
            }
            ResultAction::Ignore => {}
        }
    }

    fn set_recording(&self, recording: bool) {
        if let Some(tray) = self.tray.as_ref() {
            tray.set_recording(recording);
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if cause == StartCause::Init && self.tray.is_none() {
            // Resident, idle, power-efficient: sleep until an event wakes us.
            event_loop.set_control_flow(ControlFlow::Wait);
            match Tray::new(self.initial_active) {
                Ok(tray) => {
                    for (title, body) in self.pending_notices.drain(..) {
                        tray.notify(&title, &body);
                    }
                    self.tray = Some(tray);
                }
                Err(e) => {
                    logging::show_error_dialog("Xiuxiu — tray error", &e.to_string());
                    event_loop.exit();
                }
            }
        }
    }

    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _event: WindowEvent,
    ) {
        // No window — nothing to handle.
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Hotkey(e) => self.on_hotkey(e),
            UserEvent::Menu(e) => self.on_menu(e, event_loop),
            UserEvent::AudioCaptured(c) => self.on_audio_captured(c),
            UserEvent::Transcribed { id, result } => self.on_transcribed(id, result),
            UserEvent::TrayIcon(_) => {} // left-click etc. — unused
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Bootstrap logging + panic hook, then run the app. Any startup failure is
/// surfaced here — as a dialog AND a log line — while the `tracing-appender`
/// log guard is still alive, so a failure is never a silent exit (R1).
///
/// Surfacing lives here, NOT in `main`: the `WorkerGuard` returned by
/// `logging::init()` drops when `run()` returns, which flushes and stops the
/// background log writer. Logging from `main` (after `run()` returns) would
/// reach the dialog but be dropped from `xiuxiu.log` (KTD1).
pub fn run() -> anyhow::Result<()> {
    let _log_guard = logging::init();
    logging::install_panic_hook();
    tracing::info!("xiuxiu starting");

    let result = run_inner();
    if let Err(e) = &result {
        // Single surfacing point — the `{:#}` alternate form prints anyhow's
        // full context chain (which step failed + the underlying OS error).
        tracing::error!("startup failed: {e:#}");
        logging::show_error_dialog("Xiuxiu — startup error", &format!("{e:#}"));
    }
    result
}

/// Performs the startup work, returning any failure via `?` with `.context()`
/// so the single surfacing point in [`run`] can name the failing step (R2).
fn run_inner() -> anyhow::Result<()> {
    let raw = config::load().context("loading configuration")?;
    let (backends, fallback_notice) =
        Backends::initialize(&raw).context("initializing the transcription backend")?;

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("building the winit event loop")?;
    let proxy = event_loop.create_proxy();

    // Forward every external event source into the loop via the proxy so the
    // loop is woken and dispatches them in `user_event` (KTD3).
    {
        let p = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(UserEvent::TrayIcon(e));
        }));
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(UserEvent::Menu(e));
        }));
        let p = proxy.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(UserEvent::Hotkey(e));
        }));
    }

    // Hotkey manager + registration must happen on the event-loop thread
    // (Windows). The manager is held by App for the program lifetime. The
    // registration context (R3) names the combo and its most likely failure
    // cause, since Ctrl+Shift+Space is often already held by another app.
    let hotkey_manager =
        GlobalHotKeyManager::new().context("creating the global hotkey manager")?;
    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space);
    hotkey_manager.register(hotkey).context(
        "registering the global hotkey Ctrl+Shift+Space — it may already be in use by \
         another application (for example a Windows IME layout switch or PowerToys)",
    )?;
    let hotkey_id = hotkey.id();

    // Audio thread: posts finalized capture back as a UserEvent (KTD12). The
    // sink decouples capture.rs from the event enum.
    let audio_proxy = proxy.clone();
    let (audio, init) = AudioHandle::spawn(Box::new(move |captured| {
        let _ = audio_proxy.send_event(UserEvent::AudioCaptured(captured));
    }));

    let mut pending_notices = Vec::new();
    if let Err(e) = init {
        // R13: mic missing at launch (non-fatal — surfaced as a tray notice).
        pending_notices.push(("Microphone unavailable".to_string(), e.to_string()));
    }
    if let Some(notice) = fallback_notice {
        // R14: backend fallback.
        pending_notices.push(("Backend fallback".to_string(), notice));
    }

    let initial_active = backends.active_kind();
    let mut app = App {
        machine: Machine::new(),
        tray: None,
        backends,
        audio,
        _hotkey_manager: hotkey_manager,
        hotkey_id,
        proxy,
        initial_active,
        pending_notices,
    };

    event_loop
        .run_app(&mut app)
        .context("running the winit event loop")?;
    tracing::info!("xiuxiu exiting");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_from_idle_starts_recording() {
        let mut m = Machine::new();
        assert_eq!(m.on_press(), PressAction::StartCapture);
        assert_eq!(m.state(), AppState::Recording);
    }

    #[test]
    fn full_happy_path_idle_to_inject() {
        let mut m = Machine::new();
        assert_eq!(m.on_press(), PressAction::StartCapture);
        assert_eq!(m.on_release(), ReleaseAction::StopCapture);
        assert_eq!(m.state(), AppState::Finalizing);
        let action = m.on_captured(true);
        assert_eq!(action, CapturedAction::Dispatch(1));
        assert_eq!(m.state(), AppState::Transcribing);
        assert_eq!(m.on_transcribed(1), ResultAction::Current);
        assert_eq!(m.state(), AppState::Idle);
    }

    #[test]
    fn clip_failing_gate_returns_to_idle_without_dispatch() {
        // R4
        let mut m = Machine::new();
        m.on_press();
        m.on_release();
        assert_eq!(m.on_captured(false), CapturedAction::DropClip);
        assert_eq!(m.state(), AppState::Idle);
    }

    #[test]
    fn press_while_recording_is_ignored() {
        // R5
        let mut m = Machine::new();
        m.on_press();
        assert_eq!(m.on_press(), PressAction::Ignore);
        assert_eq!(m.state(), AppState::Recording);
    }

    #[test]
    fn press_while_finalizing_or_transcribing_is_ignored() {
        // R5
        let mut m = Machine::new();
        m.on_press();
        m.on_release(); // Finalizing
        assert_eq!(m.on_press(), PressAction::Ignore);
        m.on_captured(true); // Transcribing
        assert_eq!(m.on_press(), PressAction::Ignore);
    }

    #[test]
    fn stale_transcription_result_is_dropped() {
        // R5 / KTD7 backstop
        let mut m = Machine::new();
        m.on_press();
        m.on_release();
        m.on_captured(true); // gen_id = 1, Transcribing
        assert_eq!(m.on_transcribed(0), ResultAction::Stale);
        assert_eq!(m.state(), AppState::Idle);
    }

    #[test]
    fn audio_captured_while_idle_is_ignored() {
        let mut m = Machine::new();
        assert_eq!(m.on_captured(true), CapturedAction::Ignore);
        assert_eq!(m.state(), AppState::Idle);
    }

    #[test]
    fn release_while_idle_is_noop() {
        let mut m = Machine::new();
        assert_eq!(m.on_release(), ReleaseAction::Ignore);
        assert_eq!(m.state(), AppState::Idle);
    }

    #[test]
    fn switch_allowed_only_while_idle() {
        // R7
        let mut m = Machine::new();
        assert!(m.can_switch());
        m.on_press();
        assert!(!m.can_switch());
    }

    #[test]
    fn gen_id_increments_per_recording() {
        let mut m = Machine::new();
        m.on_press();
        m.on_release();
        assert_eq!(m.on_captured(true), CapturedAction::Dispatch(1));
        m.on_transcribed(1);
        m.on_press();
        m.on_release();
        assert_eq!(m.on_captured(true), CapturedAction::Dispatch(2));
    }
}
