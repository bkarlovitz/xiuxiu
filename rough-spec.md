# Xiuxiu — Voice Dictation Tray App

> Named after *xiuxiuejar*, the Catalan word for whisper.

## Overview

A Windows system tray app that captures microphone audio while a global hotkey is held, sends it to a transcription backend on release, and injects the transcribed text into the currently focused window. Stays resident, zero UI beyond the tray icon.

---

## Crates

- `cpal` — audio capture from default input device
- `global-hotkey` — global hotkey registration (Windows)
- `tray-icon` + `winit` — system tray icon and event loop
- `hound` — write captured audio to WAV buffer for upload
- `reqwest` with `multipart` — POST to Groq API
- `whisper-rs` — Rust bindings to whisper.cpp for local inference
- `enigo` — simulate keyboard input to inject text
- `tokio` — async runtime
- `serde` / `serde_json` — deserialize Groq response
- `dotenv` or env var — config/secrets

---

## Behavior

- On launch: register global hotkey (default `Ctrl+Shift+Space`), load configured backend, show tray icon, sit idle
- Hotkey down: begin recording from default input device via `cpal`, buffer raw PCM
- Hotkey up: stop recording, encode buffer to WAV in memory, dispatch to active backend
- On response: extract transcribed text, use `enigo` to type it into the focused window
- Tray right-click menu: backend switcher + Quit
- No window, no console in release build (`#![windows_subsystem = "windows"]`)

---

## Transcription Backends

Two backends, switchable via tray menu at runtime.

### Local (whisper.cpp)

- Uses `whisper-rs` with a ggml model file on disk (e.g. `ggml-base.en.bin` or `ggml-small.en.bin`)
- Model path set via config
- Runs synchronously on a dedicated thread (whisper.cpp is blocking), result sent back to main loop via channel
- Model loaded once at startup and kept in memory — not reloaded per invocation

### Groq API

- `POST https://api.groq.com/openai/v1/audio/transcriptions`
- `Authorization: Bearer $GROQ_API_KEY`
- Multipart form: `file` (wav bytes), `model: whisper-large-v3-turbo`, `response_format: json`
- Response shape: `{ "text": "..." }`
- Handled async via `reqwest` + `tokio`

Both backends produce the same output shape — the text injection path is identical regardless of which is active.

---

## Config

| Variable | Description |
|---|---|
| `BACKEND` | `local` or `groq` — sets default on launch |
| `GROQ_API_KEY` | Required when backend is `groq` |
| `WHISPER_MODEL_PATH` | Path to ggml model file, required when backend is `local` |

Loaded from env or a `.env` file at binary location.

---

## Tray Menu

```
Backend
  ✓ Local
    Groq
Quit
```

Checkmark on active backend. Clicking switches live.

---

## Error Handling

- API call fails: silently drop, optionally flash tray icon
- Mic not available at launch: tray notification
- Local backend selected but model file missing or failed to load: fall back to Groq, show tray notification

---

## Build

- Target: `x86_64-pc-windows-msvc`
- Release build with `#![windows_subsystem = "windows"]` to suppress console window

---

## Architecture Notes

- `tray-icon` requires a `winit` event loop on the main thread — event loop runs on main thread, async Groq calls spawned onto tokio, local inference runs on a dedicated blocking thread with results passed back via channel
- `whisper-rs` requires `libclang` and a C++ compiler (MSVC) at build time — document in README
- ggml model file is not bundled — README should link to model downloads on Hugging Face

---

## Out of Scope (v1)

- Filler word removal (Groq's model handles most of this naturally)
- Custom vocabulary
- Streaming / partial results
- Multi-hotkey profiles
- WSL-specific anything — runs on the Windows side, works in any focused window including Windows Terminal
- macOS support — out of scope for v1; crate choices should remain cross-platform compatible where possible to avoid blocking a future Mac port
