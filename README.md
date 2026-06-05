# Xiuxiu

> Named after *xiuxiuejar*, the Catalan word for whisper.

**Push-to-talk voice dictation for Windows.** Hold a hotkey, talk, release — your
words are typed into whatever window has focus. Resident in the system tray with
no window and no console. Transcribe in the cloud (Groq) or fully offline
(whisper.cpp), switchable at runtime.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform: Windows](https://img.shields.io/badge/platform-Windows-0078D6.svg)](#)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-dea584.svg)](https://www.rust-lang.org/)

---

## How it works

- **Hold** `Ctrl+Shift+Space`, speak, **release** → the transcript is typed at
  your cursor, in any focused application including the terminal.
- Two transcription backends, switchable from the tray menu without restarting:
  - **Groq** — fast hosted Whisper (`whisper-large-v3-turbo`). Needs an API key;
    audio is uploaded over HTTPS.
  - **Local** — fully offline whisper.cpp inference via `whisper-rs`. No audio
    leaves the machine.
- The microphone is open **only while the key is held** — the OS mic indicator
  and the tray icon both reflect this. Xiuxiu does not listen at idle.

```
Backend
  [x] Local
  [ ] Groq
---
Quit
```

> **Status: early.** v1 is feature-complete and unit-tested, but it has not yet
> been hardware-tested across a wide range of Windows machines. Expect rough
> edges — bug reports and ideas are very welcome (see
> [Issues & feedback](#issues--feedback)).

## Platform

Windows-only (`x86_64-pc-windows-msvc`). The code is kept cross-platform-friendly
to keep a future macOS port open, but v1 is built, tested, and run on Windows.

## Install / Build

You need a Windows machine with:

1. **Rust** with the MSVC toolchain: `rustup default stable-x86_64-pc-windows-msvc`
2. **Visual Studio Build Tools** with the *Desktop development with C++* workload
   (the MSVC C++ compiler and Windows SDK).
3. For the **local** backend, `whisper-rs` compiles bundled whisper.cpp via
   `bindgen` + CMake, so you also need:
   - **LLVM** installed, with **`LIBCLANG_PATH`** pointing at the directory
     containing `libclang.dll` (e.g. `C:\Program Files\LLVM\bin`). `bindgen`
     fails without it.
   - **CMake** on `PATH`.

   These apply even for Groq-only use because the `whisper-rs` dependency is
   always compiled. (Putting the local backend behind a Cargo feature for
   Groq-only builds is a known idea — see [Issues & feedback](#issues--feedback).)

Then:

```powershell
cargo build --release
```

The release binary is `target\release\xiuxiu.exe` and runs with no console
window. Debug builds (`cargo build`) keep a console for development output.

## Configuration

Settings come from environment variables or a `.env` file. Resolution
precedence (highest first):

1. real environment variables
2. `%APPDATA%\xiuxiu\.env`  ← **recommended** (user-profile, not world-readable)
3. `<directory containing xiuxiu.exe>\.env`  ← portable / single-user installs

| Variable | Description |
|---|---|
| `BACKEND` | `local` or `groq` — sets the default on launch. Defaults to `groq`. |
| `GROQ_API_KEY` | Required when the active backend is `groq`. |
| `WHISPER_MODEL_PATH` | Absolute path to a ggml model file, required for `local`. |
| `HOTKEY` | Push-to-talk hotkey. Defaults to `Ctrl+Alt+Space`. |

Copy [`.env.example`](.env.example) to `.env` and fill it in.

**Hotkey syntax.** `HOTKEY` is modifiers + a key joined by `+` (case-insensitive):
modifiers `Ctrl`/`Control`, `Alt`/`Option`, `Shift`, `Super`/`Cmd`/`Command`
(`Win`/`Meta` are **not** accepted — use `Super`); keys are names like `Space`, or
letters as `A`/`KeyA`. Examples: `Ctrl+Alt+Space`, `Ctrl+Alt+J`. **If the default
is already in use** by another app (Windows IME layout-switch, PowerToys, …), the
app exits at startup with a message saying so — set `HOTKEY` to a free
combination. *AltGr caveat:* on keyboard layouts where Right-Alt is AltGr (many
non-US layouts), AltGr synthesizes Ctrl+Alt, so prefer a combo without `Alt`
(e.g. `Ctrl+Shift+J`) on those keyboards.

> **Security:** `GROQ_API_KEY` is a billable credential. Do **not** place a
> populated `.env` in a world-readable directory (a shared drive, or
> `Program Files` on a multi-user machine). Prefer `%APPDATA%\xiuxiu\` or a
> user-profile install. The runtime log (`xiuxiu.log`) is written next to the
> executable and shares that directory, so the same guidance applies.

## Models (local backend)

The ggml model is **not** bundled. Download one from Hugging Face and point
`WHISPER_MODEL_PATH` at it:

- `ggml-base.en.bin` (good balance) —
  <https://huggingface.co/ggerganov/whisper.cpp/blob/main/ggml-base.en.bin>
- `ggml-small.en.bin` (more accurate, slower) —
  <https://huggingface.co/ggerganov/whisper.cpp/blob/main/ggml-small.en.bin>

If `local` is selected but the model can't load, Xiuxiu falls back to `groq`
(when a key is present) and shows a notification. A runtime switch to `local`
with no loadable model is refused, keeping the current backend.

## Behavior & caveats

- **No console / logging.** Release builds suppress the console, so all
  diagnostics go to `xiuxiu.log` next to the executable, and a crash pops a
  message box. Transcribed text and the API key are **never** written to the log.
- **Synthetic input.** Text is injected as keystrokes. Some windows reject
  synthetic input — elevated (admin) windows, certain games / anti-cheat, and
  some RDP sessions.
- **Control characters are stripped.** Newlines and escape sequences in a
  transcript are removed before injection, so dictating into a terminal cannot
  accidentally execute a command.
- **Focus matters.** The transcript lands in whatever window has focus when
  transcription *completes*. If you switch windows during a slow cloud call, the
  text follows your focus — avoid switching to a password field mid-call.
- **Groq data handling.** With the `groq` backend, audio is uploaded to Groq.
  Use the `local` backend if nothing should leave the machine.

## Architecture

A single winit event loop on the main thread coordinates every source — the
global hotkey, the tray menu, finalized audio buffers, and transcription
results — through one `UserEvent`. Audio capture runs on a dedicated thread
(the `cpal` stream is `!Send` and must not block the UI); transcription runs on
a worker thread and posts its result back to the loop.

```
src/
  main.rs            entry; sets windows_subsystem, calls run()
  lib.rs             module declarations
  config.rs          .env precedence, validation, secret redaction
  logging.rs         file logging, panic hook, crash dialog
  app.rs             event loop, recording state machine, run()
  tray.rs            tray icon + menu, idle/recording icon
  inject.rs          Unicode text injection (enigo), sanitize + chunk
  audio/
    capture.rs       dedicated audio thread, build-on-press, command channel
    preprocess.rs    downmix, rubato resample, silence/duration gate
  transcription/
    mod.rs           ActiveBackend enum, dispatch, startup fallback / switching
    groq.rs          Groq multipart upload + error mapping
    local.rs         whisper.cpp via whisper-rs
tests/
  config_env.rs      integration: load config from a real .env
```

The full design rationale lives in
[`docs/plans/`](docs/plans/2026-06-04-001-feat-xiuxiu-voice-dictation-tray-app-plan.md).

## Testing

```powershell
cargo test
```

Pure logic (config precedence, audio math, WAV encoding, Groq error mapping,
sanitize/chunk, state-machine transitions) is covered by unit tests. Hardware /
OS integration (real microphone capture, the global hotkey, tray rendering, and
keystroke injection) is verified manually on Windows.

## Issues & feedback

This is a personal project with a single maintainer. **Bug reports, ideas, and
questions are welcome — please [open an issue](../../issues).** Code
contributions (pull requests) are **not** accepted; `main` is maintained solely
by the author. If you want to build on Xiuxiu, fork it — that's what the MIT
license is for.

Ideas already on the radar: putting the local backend behind a Cargo feature
(Groq-only builds without the LLVM/CMake chain), a configurable hotkey, and
remembering the last-selected backend across restarts.

## License

[MIT](LICENSE) © Bryan Karlovitz

## Acknowledgements

Built on the excellent work of
[whisper.cpp](https://github.com/ggerganov/whisper.cpp) /
[whisper-rs](https://github.com/tazz4843/whisper-rs),
[Groq](https://groq.com/),
[cpal](https://github.com/RustAudio/cpal),
[winit](https://github.com/rust-windowing/winit),
[tray-icon](https://github.com/tauri-apps/tray-icon),
[global-hotkey](https://github.com/tauri-apps/global-hotkey),
[enigo](https://github.com/enigo-rs/enigo),
[rubato](https://github.com/HEnquist/rubato), and
[hound](https://github.com/ruuda/hound).
