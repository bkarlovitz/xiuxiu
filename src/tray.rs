//! System tray icon + menu (R3, R7).
//!
//! The menu is:
//! ```text
//! Backend
//!   [x] Local
//!   [ ] Groq
//! ---
//! Quit
//! ```
//! The icon distinguishes idle from recording (R3) so the user always knows
//! when the microphone is live.
//!
//! Note on notifications: `tray-icon` does not expose cross-platform balloon
//! notifications, so user-facing notices use a native message box
//! ([`crate::logging::show_error_dialog`]) for rare must-see events, and the
//! tooltip + log for transient ones (e.g. a dropped API call, R12).

use tray_icon::menu::{CheckMenuItem, Menu, MenuId, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::config::Backend;
use crate::logging;

const ICON_SIZE: u32 = 32;

pub struct Tray {
    // Kept alive for the program's lifetime — dropping removes the tray icon.
    tray: TrayIcon,
    idle_icon: Icon,
    recording_icon: Icon,
    check_local: CheckMenuItem,
    check_groq: CheckMenuItem,
    quit_id: MenuId,
    local_id: MenuId,
    groq_id: MenuId,
}

impl Tray {
    pub fn new(active: Backend) -> anyhow::Result<Tray> {
        let check_local = CheckMenuItem::new("Local", true, active == Backend::Local, None);
        let check_groq = CheckMenuItem::new("Groq", true, active == Backend::Groq, None);
        let backend_menu = Submenu::with_items("Backend", true, &[&check_local, &check_groq])?;
        let separator = PredefinedMenuItem::separator();
        let quit = tray_icon::menu::MenuItem::new("Quit", true, None);

        let menu = Menu::with_items(&[&backend_menu, &separator, &quit])?;

        let idle_icon = solid_icon(0x6c, 0x6c, 0x6c); // gray
        let recording_icon = solid_icon(0xd6, 0x2c, 0x2c); // red

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Xiuxiu — idle")
            .with_icon(idle_icon.clone())
            .build()?;

        Ok(Tray {
            tray,
            idle_icon,
            recording_icon,
            quit_id: quit.id().clone(),
            local_id: check_local.id().clone(),
            groq_id: check_groq.id().clone(),
            check_local,
            check_groq,
        })
    }

    /// Returns `true` if the given menu id is the Quit item.
    pub fn is_quit(&self, id: &MenuId) -> bool {
        id == &self.quit_id
    }

    /// Maps a clicked menu id to a backend selection, if it is one.
    pub fn backend_for(&self, id: &MenuId) -> Option<Backend> {
        if id == &self.local_id {
            Some(Backend::Local)
        } else if id == &self.groq_id {
            Some(Backend::Groq)
        } else {
            None
        }
    }

    /// Reflect the active backend in the checkmarks (R7).
    pub fn set_active(&self, active: Backend) {
        self.check_local.set_checked(active == Backend::Local);
        self.check_groq.set_checked(active == Backend::Groq);
    }

    /// Swap the icon + tooltip to reflect recording vs idle (R3).
    pub fn set_recording(&self, recording: bool) {
        let icon = if recording {
            self.recording_icon.clone()
        } else {
            self.idle_icon.clone()
        };
        let _ = self.tray.set_icon(Some(icon));
        let _ = self.tray.set_tooltip(Some(if recording {
            "Xiuxiu — recording"
        } else {
            "Xiuxiu — idle"
        }));
    }

    /// Must-see notification (mic missing, fallback, switch rejected). Uses a
    /// native dialog since tray balloons aren't available cross-platform.
    pub fn notify(&self, title: &str, body: &str) {
        tracing::info!("{title}: {body}");
        logging::show_error_dialog(title, body);
    }

    /// Transient, low-severity notice (e.g. a dropped API call, R12). Log +
    /// tooltip only — no modal interruption.
    pub fn transient(&self, message: &str) {
        tracing::warn!("{message}");
        let _ = self.tray.set_tooltip(Some(&format!("Xiuxiu — {message}")));
    }
}

/// Build a solid-color square icon. Placeholder art — swap in real `.ico` assets
/// later; generating in code avoids shipping binary assets for v1.
fn solid_icon(r: u8, g: u8, b: u8) -> Icon {
    let mut rgba = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    for _ in 0..(ICON_SIZE * ICON_SIZE) {
        rgba.extend_from_slice(&[r, g, b, 0xff]);
    }
    Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE).expect("valid solid icon")
}
