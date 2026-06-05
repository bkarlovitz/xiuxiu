---
title: "fix: surface silent startup failures (and harden hotkey registration)"
type: fix
status: active
date: 2026-06-04
---

# fix: surface silent startup failures (and harden hotkey registration)

## Summary

After the whisper model loads, `xiuxiu.exe` exits with code 1 and **no diagnostic** — no panic, no message box, no log line. The cause is a diagnostics gap: `main.rs` discards the error returned by `run()`, and the post-initialization steps in `run()` propagate errors via `?` without surfacing them. Fix the gap so *any* startup failure shows a dialog + log entry naming the failing step, then give the leading suspect (global-hotkey registration of `Ctrl+Shift+Space`) a clear, actionable message.

## Problem Frame

Observed on Windows:

```
whisper_model_load: model size = 147.37 MB
error: process didn't exit successfully: `target\debug\xiuxiu.exe` (exit code: 1)
```

The model loads (so `config::load` and `Backends::initialize` both succeeded — those are the only two paths that currently call `show_error_dialog`). Execution then reaches a step that returns an error, and the error vanishes:

- `src/main.rs` is `if xiuxiu::run().is_err() { std::process::exit(1); }` — the error value is dropped; nothing is printed, logged, or shown.
- In `src/app.rs::run`, the steps after `Backends::initialize` use bare `?` with no dialog/log: `EventLoop::with_user_event().build()?`, `GlobalHotKeyManager::new()?`, `hotkey_manager.register(hotkey)?`, and `event_loop.run_app(&mut app)?`.

So a failure in any of those four steps is silent. (Tray-creation failure inside `new_events` is *not* the culprit: that path already calls `show_error_dialog` then `event_loop.exit()`, which would have shown a dialog and exited 0.)

The exit happens immediately after model load, before any tray icon appears, which points at one of the pre-`run_app` steps. The leading hypothesis is `GlobalHotKeyManager::register(Ctrl+Shift+Space)` failing because another process already holds that combination (Windows IME layout-switch, PowerToys, or a similar global-hotkey owner). But the precise cause cannot be confirmed from this Linux dev box — it is only observable on the user's Windows machine — which is exactly why the diagnostics fix comes first: once errors are visible, the real cause names itself.

## Requirements

- R1. Any startup failure surfaces to the user via the message box **and** the log file — never a silent exit with code 1.
- R2. The surfaced message identifies the failing step and includes the underlying error (so the cause is actionable without a debugger).
- R3. A global-hotkey registration failure produces a clear message naming the hotkey and the likely "already in use by another app" cause.

## Key Technical Decisions

- KTD1. **Centralize startup-error surfacing inside `run()`, where the log guard is still alive.** Split `run()` into an inner function that does the work and returns errors via `?`, and an outer `run()` that calls it and — on `Err`, *while the `tracing-appender` `WorkerGuard` from `logging::init()` is still in scope* — routes the error through `logging::show_error_dialog` + `tracing::error!`, then returns the error. `main` stays a thin `if run().is_err() { exit(1) }`. **Surfacing must NOT move to `main`** (after `run()` returns): the `WorkerGuard` drops when `run()` returns, which flushes and stops the background log writer, so a `tracing::error!` in `main` would reach the dialog but be dropped from `xiuxiu.log` — half-defeating R1 in the exact failure mode this plan fixes. This single surfacing point makes *every* startup failure visible regardless of which step failed; it does not depend on guessing the cause. `show_error_dialog` already exists and works in both debug and release builds.
- KTD2. **Name the step with `anyhow` context.** Add `.context(...)` (anyhow) to each fallible startup step in `run()` so the surfaced message reads, e.g., "registering global hotkey Ctrl+Shift+Space: <os error>" rather than a bare error. This turns the generic dialog into a pointer at the exact failure.
- KTD3. **Hotkey registration is the leading hypothesis; treat it explicitly.** Give `GlobalHotKeyManager::new()`/`register()` a dedicated, actionable message (name the combo, note it may be held by another app such as an IME or PowerToys). Keep it fatal for now — without the hotkey the app is unusable — but fatal *with a clear message*, not a silent exit. A configurable hotkey is already deferred in the app plan and stays deferred here.

## Implementation Units

### U1. Surface all startup errors (stop the silent exit)

- **Goal:** Make any error returned by `run()` produce a message box + log entry that names the failing step, instead of a silent `exit(1)`.
- **Requirements:** R1, R2.
- **Dependencies:** none.
- **Files:** `src/main.rs` (modify), `src/app.rs` (modify — the `run` function).
- **Approach:** Per KTD1/KTD2.
  - In `src/app.rs`, split the current `run()` body into an inner function (e.g. `run_inner() -> anyhow::Result<()>`) that does the work and returns errors via `?`. Keep `let _log_guard = logging::init();` and `install_panic_hook()` in the **outer** `run()` so the guard outlives the inner call. Outer `run()`: `let result = run_inner(...);` then `if let Err(e) = &result { logging::show_error_dialog("Xiuxiu — startup error", &format!("{e:#}")); tracing::error!("startup failed: {e:#}"); }` and `return result;`. Use the `{:#}` alternate form so anyhow prints the full context chain. (Passing the proxy/guard between outer and inner is a mechanical detail for the implementer.)
  - **Collapse the existing per-path dialogs into this one point.** The `config::load` and `Backends::initialize` error arms currently call `show_error_dialog` inline; with the single surfacing point they should just `return Err(...)` (with `.context(...)`) so there is exactly one dialog and one log line — no double dialogs.
  - Add `.context("...")` to each remaining fallible startup step inside `run_inner`: building the event loop, creating the `GlobalHotKeyManager`, registering the hotkey, and `run_app`. Keep the messages short and specific to the step.
  - `src/main.rs` stays the thin `if xiuxiu::run().is_err() { std::process::exit(1); }` — no surfacing in `main` (the guard is already dropped there; see KTD1).
- **Patterns to follow:** the existing `logging::show_error_dialog` usage in `run()`; `anyhow::Context`; the existing `_log_guard` lifetime note in `logging.rs`.
- **Test scenarios:** `Test expectation: none — this is startup/IO/event-loop glue with no pure-logic seam. Verified by running on Windows (below).`
- **Verification:** On Windows, the run that previously printed only `exit code: 1` now pops a message box (and writes to `xiuxiu.log`) naming the failing step and the underlying error.

### U2. Actionable global-hotkey registration error

- **Goal:** When registering `Ctrl+Shift+Space` fails, surface a clear, actionable message rather than a generic one.
- **Requirements:** R3.
- **Dependencies:** U1 (relies on U1's surfacing path to actually show the message).
- **Files:** `src/app.rs` (modify — the hotkey-manager creation/registration in `run`).
- **Approach:** Per KTD3. Give the `GlobalHotKeyManager::new()` and `register(hotkey)` steps a context message that names the hotkey (`Ctrl+Shift+Space`) and notes the likely cause ("the shortcut may already be in use by another application, e.g. a Windows IME or PowerToys"). Registration stays fatal (the app needs the hotkey), but now exits with that message via U1's path.
- **Patterns to follow:** the `anyhow::Context` usage added in U1.
- **Test scenarios:** `Test expectation: none — registration is an OS call; verified on Windows.`
- **Verification:** If hotkey registration is the actual cause, the re-run shows the actionable message; the user can free the combo (or close the conflicting app) and the app proceeds to the tray.

## Verification & Notes

- **The real root cause is confirmed at runtime on Windows.** U1 is the load-bearing fix: it makes the currently-invisible failure visible. Run `cargo run` again after U1 — the message box will name the failing step:
  - If it names **hotkey registration**, U2's message guides the fix (free `Ctrl+Shift+Space`); confirm the app then reaches the tray.
  - If it names a **different step** (event-loop build or `run_app`), fix that step — it is now identified, with its OS error attached, and can be addressed directly (or via `/ce-debug` with the concrete error in hand).
- This is source-only; no dependency or config changes.

## Scope Boundaries

- In scope: surfacing startup errors (the diagnostics gap) and an actionable message for the leading-suspect hotkey-registration failure.
- Deferred to follow-up work: a configurable hotkey (already deferred in the app plan) — the proper long-term fix if `Ctrl+Shift+Space` is chronically contended on the user's machine.
- Out of scope: changing the recording/transcription flow or any behavior beyond startup error handling.
