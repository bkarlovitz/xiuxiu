// Suppress the console window in release builds. Debug builds keep a console so
// `tracing` output and panics are visible during development. With the console
// suppressed, all diagnostics go to the log file + tray/MessageBox (see logging.rs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `run` installs the panic hook and logging first, then surfaces any
    // startup failure to the user (tray notification / message box) itself.
    // The exit code is the only thing left to set here.
    if xiuxiu::run().is_err() {
        std::process::exit(1);
    }
}
