---
title: "fix: reliable text injection (throttled typing) + silence whisper.cpp logging"
type: fix
status: active
date: 2026-06-04
---

# fix: reliable text injection (throttled typing) + silence whisper.cpp logging

## Summary

Two post-launch quality problems, both downstream of a confirmed-correct capture→whisper chain (the model transcribes accurately in the logs). First, text injection drops/garbles characters because `enigo.text()` sends the whole string as one `SendInput` burst that outruns target apps; fix it by typing in small throttled batches with a configurable per-character delay. Second, whisper.cpp floods the console with its own internal log stream (not covered by the `set_print_*` flags); silence it with `whisper_rs::install_logging_hooks()` at startup.

## Problem Frame

The user ran the local backend and confirmed (from the whisper debug logs) that transcription is **correct** — e.g. whisper produced "Testing that this works." and "Tomorrow morning I definitely need to finish the app…". But two things are wrong downstream:

1. **Injection garbles correct text.** What lands in the focused window is mangled: "morning" → "orning", "this streamed the characters as I'm speaking." truncated to "this streamed ", whole phrases collapsing to a first word plus runs of spaces. Root cause (confirmed against enigo 0.6.1 source): `Keyboard::text()` collects the entire string into a single `Vec<INPUT>` and calls `SendInput` **once**, with no inter-key delay. Terminals/editors that pump input slower than the burst drop characters. `src/inject.rs` currently chunks at 64 chars with a delay only *between* chunks, so each 64-char `text()` call is still a full burst. enigo 0.6.1 `Settings` has **no Windows-applicable delay field** (`linux_delay` is X11-only), so throttling must be done by the caller.

2. **whisper.cpp log flood.** Lines like `whisper_init_with_params_no_state:`, `whisper_model_load:`, and `whisper_full_with_state:` pour onto the console. These come from whisper.cpp/GGML's own log stream, which the `FullParams::set_print_*(false)` flags (already set in `src/transcription/local.rs`) do **not** control. whisper-rs 0.16.0 exposes exactly one function for this — `install_logging_hooks()` — which redirects that stream away from stderr; with no backend feature enabled it drops the messages entirely.

Neither is an audio or model problem. Build/verification is on Windows (this dev box is Linux and cannot compile the native crates), so the approach is grounded in the enigo 0.6.1 and whisper-rs 0.16.0 docs/source.

## Requirements

- R1. Injecting a correct transcript into a focused Windows app (incl. Windows Terminal, editors) reproduces the full text without dropped, reordered, or truncated characters.
- R2. Full Unicode (including non-ASCII and emoji) is typed correctly.
- R3. The typing throttle is tunable without a rebuild, via a `TYPING_DELAY_MS` setting (same config precedence as the other settings), with a sensible default.
- R4. whisper.cpp/GGML internal logging no longer prints to the console.
- R5. The existing control-character/NUL sanitization (terminal safety) is preserved.

## Key Technical Decisions

- KTD1. **Reliable injection = throttled per-character `text()`, not one burst.** enigo 0.6.1 has no built-in Windows inter-key delay and `text()` sends the whole string in a single `SendInput` (the drop cause). The fix is to iterate the (sanitized) text by `char` and call `enigo.text(&c.to_string())` for each, sleeping a small configurable delay between. Use `text()` (the pure `KEYEVENTF_UNICODE` path), **not** `key(Key::Unicode(c), Click)` — the latter attempts a virtual-key mapping first (wrong for modifier-needing chars, plus a `warn!` per char). Passing whole `char`s lets enigo's `encode_utf16` reconstruct surrogate pairs, so emoji/non-BMP work (R2).
- KTD2. **Per-character delay is configurable: `TYPING_DELAY_MS`, default 8 ms.** Target apps vary in how fast they accept synthetic input, and the research confirms the delay is the real lever; making it an env/`.env` setting (mirroring the existing config pattern) lets the user tune it without recompiling. 8 ms is a reasonable default; the existing 64-char/10-ms-between-chunks scheme is replaced. (Batching >1 char per `text()` call is possible for speed, but v1 goes char-by-char for maximum reliability; raising the batch size is a future tuning option.)
- KTD3. **Silence whisper.cpp logging with `whisper_rs::install_logging_hooks()` at startup.** Called once before any `WhisperContext` is created (global, idempotent). With whisper-rs's default features (no `log_backend`/`tracing_backend`), the captured messages are **dropped** — the minimal, dependency-free fix. Routing them into our `tracing` file logger instead (via the `tracing_backend` feature) is a deferred option if whisper diagnostics are ever wanted; for now the goal is silence.
- KTD4. **Char-level batching for v1; grapheme atomicity deferred.** Iterating by `char` can split a multi-`char` grapheme cluster (a combining accent, a ZWJ emoji sequence) into separate Unicode events. For dictation output this is acceptable; if it ever matters, batch by grapheme cluster via `unicode-segmentation`. Not pulled into v1 (no dependency added).
- KTD5. **Injection runs on the transcription worker thread, never the event-loop thread.** Throttled per-character typing takes ~`char_count × delay` ms (e.g. ~1.6 s for 200 chars at 8 ms). `inject` is currently called inline in `on_transcribed`, which runs on the winit event-loop (UI) thread — so that sleep would freeze the tray, hotkey, and all events for the whole duration. Move injection into the worker that `dispatch()` already spawns for transcription: the worker does `to_canonical` → `transcribe` → (on success) `inject`, then posts a completion event; the event loop stays responsive throughout. `SendInput`/enigo work from any thread on Windows, so off-thread typing is fine. Single-in-flight (the state stays busy until the worker's completion event arrives) keeps injections from overlapping, so no second injection can interleave keystrokes.

## Implementation Units

### U1. Configurable typing delay (`TYPING_DELAY_MS`)

- **Goal:** Add a `TYPING_DELAY_MS` setting (default 8) read with the standard precedence, exposed as a parsed value for the injector.
- **Requirements:** R3.
- **Dependencies:** none.
- **Files:** `src/config.rs` (modify — key constant, default, `RawConfig` field, `resolve` wiring, **`env_map()` key array**; extend inline `#[cfg(test)]`).
- **Approach:** Mirror the `HOTKEY` addition exactly. Add `KEY_TYPING_DELAY_MS = "TYPING_DELAY_MS"` and `DEFAULT_TYPING_DELAY_MS = 8`. Add a `typing_delay_ms: u64` field to `RawConfig`. In `resolve`, parse `pick(KEY_TYPING_DELAY_MS)` with `str::parse::<u64>()`, falling back to the default on absent **or unparseable** values (a bad number should not be fatal for a tuning knob — fall back and move on). Add `KEY_TYPING_DELAY_MS` to the `env_map()` array so a real env var is honored (the array currently lists `KEY_BACKEND, KEY_GROQ, KEY_MODEL, KEY_HOTKEY` — append the new key). Store the parsed `u64` (milliseconds) on `RawConfig`; the `Duration` conversion happens at the injection call site (U2), not here.
- **Patterns to follow:** the `HOTKEY`/`DEFAULT_HOTKEY` + `env_map()` additions already in `src/config.rs`.
- **Test scenarios:**
  - Covers R3. Unset → `typing_delay_ms == 8`.
  - Covers R3. `TYPING_DELAY_MS=20` → `20`.
  - Precedence holds (env > appdata > exedir) for the key.
  - A non-numeric value (`TYPING_DELAY_MS=fast`) falls back to the default rather than erroring.
- **Verification:** `cargo test` config tests pass; the value defaults to 8 and reads overrides.

### U2. Throttled, reliable text injection

- **Goal:** Replace the single-burst chunked typing with per-character `text()` calls paced by the configured delay, preserving sanitization.
- **Requirements:** R1, R2, R5.
- **Dependencies:** U1.
- **Files:** `src/inject.rs` (modify — `inject` signature + body, remove the burst constants; update inline `#[cfg(test)]`); `src/app.rs` (modify — move injection off the event-loop thread into the `dispatch` worker; carry the delay on `App`).
- **Approach:** Per KTD1/KTD4/KTD5.
  - **`inject.rs`:** keep `sanitize` (NUL + control-char stripping, R5) **unchanged**. Change `inject` to `inject(text: &str, delay: Duration)`. Sanitize first, then iterate the **sanitized** string by `char`; for each char call `enigo.text(&c.to_string())` and then `std::thread::sleep(delay)` — skip the sleep after the last char. Construct `Enigo` once per call (unchanged). **Remove** the `CHUNK_CHARS = 64` and `INTER_CHUNK_DELAY` constants and the `chunk_text` helper (direct char iteration replaces them); the pure-logic testable seam is `sanitize`, which is unchanged and stays tested. A sanitized-to-empty string types nothing.
  - **`app.rs` (the KTD5 move):** carry the delay on `App` as `typing_delay_ms: u64` (threaded from `RawConfig` in `run_inner`, same as the other config-derived `App` fields). In `dispatch` — the worker thread already spawned for transcription — after a successful `transcribe`, log the transcript length (`logging::describe_transcript`, R16) and call `inject(&text, Duration::from_millis(self.typing_delay_ms))` **on that worker thread**, then post the completion event. The per-char sleeps now run off the UI thread (KTD5). `on_transcribed` no longer types; it only resolves the state (→ Idle) and, on an error result, fires the transient notification (R12, unchanged). Pass `typing_delay_ms` into the worker closure by value (it's `Copy`).
- **Patterns to follow:** the existing `sanitize`/`inject` structure in `src/inject.rs`; the existing `dispatch` worker-thread pattern in `src/app.rs` (which already offloads transcription and posts a `UserEvent` back); the way `App` already carries config-derived values (e.g. `hotkey_id`) from `run_inner`.
- **Test scenarios:**
  - Covers R5. `sanitize` still strips NUL + control chars and preserves normal Unicode/emoji (the existing inline test stays green — `sanitize` is unchanged and is the pure-logic seam now that `chunk_text` is gone).
  - `sanitize().chars().collect::<String>()` round-trips the sanitized text (the exact sequence `inject` will type) with no loss or reorder.
  - `Test expectation: the actual SendInput typing, the per-char throttle, and the off-thread move are OS/threading-level and verified manually on Windows (below); only `sanitize` is unit-tested.`
- **Verification (manual, Windows):** Dictate a long sentence with punctuation and a non-ASCII character into Windows Terminal, Notepad, and a browser field; the full text appears intact (no "orning"/truncation/space-runs). Bumping `TYPING_DELAY_MS` higher fixes any residual drops on a stubborn target.

### U3. Silence whisper.cpp / GGML logging

- **Goal:** Stop whisper.cpp's internal log stream from printing to the console.
- **Requirements:** R4.
- **Dependencies:** none.
- **Files:** `src/app.rs` (modify — call `whisper_rs::install_logging_hooks()` once at startup).
- **Approach:** Per KTD3. Call `whisper_rs::install_logging_hooks()` in the **outer `run()`**, immediately after `logging::install_panic_hook()` and before `run_inner()` (which is where `Backends::initialize` creates the `WhisperContext`). It is global and idempotent and must run before any `WhisperContext` is created, which this ordering guarantees. No `Cargo.toml` change — with whisper-rs's default features the captured logs are dropped (silent), which is the goal.
- **Patterns to follow:** the existing startup ordering in `run()` (logging → panic hook → … ).
- **Test scenarios:** `Test expectation: none — a single global FFI hook install with no pure-logic seam; verified on Windows by the absence of whisper_*/ggml log lines on the console.`
- **Verification (manual, Windows):** `cargo run` with `BACKEND=local`; the `whisper_init_*`, `whisper_model_load:`, and `whisper_full_with_state:` lines no longer appear.

### U4. Document `TYPING_DELAY_MS`

- **Goal:** Document the new tuning setting.
- **Requirements:** R3.
- **Dependencies:** U1.
- **Files:** `.env.example` (modify), `README.md` (modify — config table + one line).
- **Approach:** Add `TYPING_DELAY_MS` (default 8) to the config table and `.env.example`, noting it's the per-character typing delay in milliseconds and that raising it helps if a target app drops characters.
- **Test scenarios:** `Test expectation: none — docs only.`
- **Verification:** A reader can find and tune `TYPING_DELAY_MS` from the README/`.env.example`.

## Verification & Notes

- End-to-end confirmation is the user's Windows `cargo run` with `BACKEND=local`: a dictated sentence types in fully and correctly into a terminal/editor, the UI stays responsive while it types (KTD5), and the whisper console flood is gone.
- **Residual hypothesis if drops persist after throttling.** Burst-rate is the confirmed primary cause, but *leading*-character loss can also be a focus/modifier-settle race — injection fires when the worker finishes, and a modifier from the hotkey (`Ctrl+Alt+…`) or unstable target focus at that instant can eat the first char(s). If raising `TYPING_DELAY_MS` does not fully cure leading-char loss, the next lever is a small one-time settle delay before the first char (and/or confirming no modifier is logically held at injection time) — not a larger per-char delay. Note this so a persistent residual isn't misread as "the throttle fix failed."
- Source-only plus possibly one `.env.example`/README line; no dependency changes (`whisper-rs install_logging_hooks` needs no feature for the silent path; injection uses existing `enigo`).

## Scope Boundaries

- In scope: throttled reliable typing with a tunable delay, and silencing whisper.cpp logging.
- Deferred to follow-up work: clipboard-paste injection mode (instant, drop-proof, but clobbers clipboard and paste-shortcut varies by app) as an opt-in alternative; grapheme-cluster-aware batching via `unicode-segmentation`; routing whisper logs into the `tracing` file logger via the `tracing_backend` feature (instead of dropping them).
- Out of scope: any change to audio capture, preprocessing, the model, or the transcription result itself (all confirmed correct).
