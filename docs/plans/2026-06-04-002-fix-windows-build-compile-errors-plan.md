---
title: "fix: Windows build compile errors (cpal 0.17 + whisper-rs 0.16 API drift)"
type: fix
status: active
date: 2026-06-04
---

# fix: Windows build compile errors (cpal 0.17 + whisper-rs 0.16 API drift)

## Summary

The first Windows build of `xiuxiu` failed with three compile errors, all from crate APIs that changed since the versions the code was written against. Fix the two affected files (`src/audio/capture.rs`, `src/transcription/local.rs`) to match the confirmed cpal 0.17.3 and whisper-rs 0.16.0 APIs. No behavior change; existing inline tests stay valid.

## Problem Frame

The plan that produced this app flagged these exact spots on a "watch-list" — the version-sensitive crate APIs that couldn't be compile-checked on the Linux planning box (the native deps don't build there). On the first real `cargo build` on `x86_64-pc-windows-msvc`, all deps compiled and only the `xiuxiu` lib failed, with three errors:

1. `src/audio/capture.rs:192` — `supported.sample_rate().0` → E0610 "`u32` is a primitive type and therefore doesn't have fields".
2. `src/transcription/local.rs:54` — `state.full_n_segments().map_err(...)?` → E0599 (`full_n_segments` returns `i32`, not `Result`).
3. `src/transcription/local.rs:58` — `state.full_get_segment_text(i)` → E0599 "no method named `full_get_segment_text`"; compiler suggested `get_segment`.

These are pure API-shape corrections. The fix is reasoned from the official docs (the Linux dev box still cannot compile the Windows-target/native crates, so verification happens on the user's Windows machine).

## Requirements

- R1. `cargo build` on `x86_64-pc-windows-msvc` compiles the `xiuxiu` lib without the three reported errors.
- R2. The local backend still returns the concatenated transcript text, using the whisper-rs 0.16 segment API.
- R3. Existing inline unit tests remain valid and unchanged in intent (`concat_segments`, the local load-failure test, the cpal `SessionBuffer` tests).

## Key Technical Decisions

- KTD1. **cpal 0.17 made `SampleRate` a `u32` type alias** (was a `SampleRate(pub u32)` newtype). `SupportedStreamConfig::sample_rate()` returns `u32` and `StreamConfig.sample_rate` is `u32`, so the `.0` is removed, not replaced. `SupportedStreamConfig::config()` is unchanged and still feeds `build_input_stream`. (Confirmed: cpal 0.17.3 source + UPGRADING.md.)
- KTD2. **whisper-rs 0.16 reworked the segment API.** `full()` still returns `Result` (keep `?`). `full_n_segments()` returns `i32` (drop the `.map_err(...)?`). Segments are accessed via `get_segment(i) -> Option<WhisperSegment>`; the flat `full_get_segment_text` no longer exists. (Confirmed: docs.rs/whisper-rs/0.16.0.)
- KTD3. **Use `WhisperSegment::to_str_lossy()` rather than `to_str()`** for the text accessor. Both return `Result`, but `to_str_lossy()` tolerates occasional non-UTF-8 bytes in model output instead of failing the whole dictation; for a dictation tool, a lossy character beats a dropped utterance.
- KTD4. **Preserve the `Vec<String>` + `concat_segments` shape.** Collecting owned segment strings and joining via the existing pure `concat_segments` helper keeps that helper's unit test valid and keeps segment borrows from outliving the `WhisperState` borrow.

## Implementation Units

### U1. Fix cpal sample-rate access in capture.rs

- **Goal:** Remove the obsolete `.0` so the native sample rate reads as a `u32` under cpal 0.17.
- **Requirements:** R1.
- **Dependencies:** none.
- **Files:** `src/audio/capture.rs` (modify — the `start_capture` function, the `let rate = supported.sample_rate().0;` line).
- **Approach:** Per KTD1, change to `let rate = supported.sample_rate();` (already a `u32`). Leave `supported.channels()` and `supported.config()` untouched — they are unchanged in 0.17. Confirm no other `.0`-on-sample-rate or `SampleRate(..)` constructions exist elsewhere in the file (there is only the one site).
- **Test scenarios:** `Test expectation: none — type-only correction, no behavioral change. The existing `SessionBuffer` accumulation/conversion tests already cover this module's logic and must still pass.`
- **Verification:** The E0610 error at `capture.rs` is gone on `cargo build` (Windows).

### U2. Fix whisper-rs segment extraction in local.rs

- **Goal:** Replace the pre-0.16 segment calls with the 0.16 `full_n_segments()` + `get_segment()` + `to_str_lossy()` API so local transcription compiles and returns text.
- **Requirements:** R1, R2, R3.
- **Dependencies:** none.
- **Files:** `src/transcription/local.rs` (modify — the segment-collection block inside `LocalWhisperBackend::transcribe`).
- **Approach:** Per KTD2/KTD3/KTD4:
  - Keep `state.full(params, audio).map_err(...)?` as-is (`full` returns `Result`).
  - Change `full_n_segments()` to a plain call (no `.map_err(...)?`): it returns `i32`.
  - Loop `for i in 0..n`, call `state.get_segment(i)`, and for each `Some(segment)` push `segment.to_str_lossy().map_err(|e| TranscribeError::Local(e.to_string()))?.into_owned()` into the existing `Vec<String>`. The `?` yields a `Cow<str>`; `.into_owned()` (or `.to_string()`) produces the owned `String` the `Vec<String>` requires and discharges the segment's borrow of `WhisperState` (KTD4).
  - Keep the final `Ok(concat_segments(&segments))`.
  - The index form (not the `as_iter()` iterator form) is the smaller diff from the current code and uses the exact methods the compiler named; either is acceptable, but the index form keeps `full_n_segments` which the code already calls.
- **Patterns to follow:** the existing `concat_segments` helper and its inline test in `src/transcription/local.rs` — do not change its signature.
- **Test scenarios:**
  - The existing `concat_joins_and_trims_segments` and `concat_handles_empty` tests for `concat_segments` remain valid unchanged (the helper is untouched).
  - The existing `try_new_on_missing_model_returns_error_not_panic` test remains valid (the load path is untouched).
  - `Test expectation: no new tests — the segment-extraction code calls into the native whisper model and can only be exercised on Windows with a real model file (already covered by the plan's manual verification: a spoken clip transcribes offline). The pure, testable seam (`concat_segments`) is unchanged.`
- **Verification:** Both E0599 errors at `local.rs` are gone; `cargo build` proceeds past the lib.

## Verification & Notes

- **Build is the verification.** After both units, run `cargo build` (and `cargo test`) on Windows. The first build stopped at the first batch of errors in the lib, so once these three are resolved the compiler may surface *further* drift not yet seen (e.g., in the `tray-icon`/`muda`, `global-hotkey`, or `enigo` call sites also flagged on the original watch-list). Treat any new errors the same way: read the compiler's message, correct the call to the installed crate's API, rebuild. This is expected for a first cross-compile and is not a defect in this plan.
- **No dependency changes.** This is source-only; `Cargo.toml`/`Cargo.lock` are untouched.

## Scope Boundaries

- In scope: the three reported compile errors and any directly-adjacent same-API sites in the two named files.
- Deferred to follow-up work: putting the local/whisper backend behind a Cargo feature (so Groq-only builds skip the LLVM/CMake chain) — a known idea, tracked separately, not part of this fix.
- Out of scope: any behavior change, new features, or refactors beyond the API corrections.
