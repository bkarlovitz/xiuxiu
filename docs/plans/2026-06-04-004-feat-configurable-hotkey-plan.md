---
title: "feat: configurable global hotkey (HOTKEY) with a less-contended default"
type: feat
status: active
date: 2026-06-04
---

# feat: configurable global hotkey (HOTKEY) with a less-contended default

## Summary

The hardcoded `Ctrl+Shift+Space` global hotkey is already claimed by another app on the user's machine, so registration fails and (now that startup errors are surfaced) the app reports `HotKey already registered` and exits. Make the hotkey configurable via a new `HOTKEY` setting parsed with `global-hotkey`'s native string parser, default it to the less-contended `Ctrl+Alt+Space`, and make both the parse-error and registration-conflict messages point the user at `HOTKEY`. Any fixed combo is hostage to whatever grabbed it first; configurability is the real fix.

## Problem Frame

Confirmed on Windows (the startup-error dialog from the prior fix):

```
registering the global hotkey Ctrl+Shift+Space — it may already be in use by
another application … : HotKey already registered:
HotKey { mods: Modifiers(CONTROL | SHIFT), key: Space, id: 34078782 }
```

The model loads and every other startup step succeeds; only hotkey registration fails because `Ctrl+Shift+Space` is held by another resident app (a Windows IME layout-switch binding, PowerToys, or similar). The hotkey is currently hardcoded in `src/app.rs::run_inner` (`HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space)`), so the only escape is a code change. A configurable hotkey — already earmarked as deferred follow-up in the app plan — lets the user pick a free combination, and a better default avoids the conflict out-of-the-box on typical machines.

## Requirements

- R1. The global hotkey is configurable via a `HOTKEY` setting read from the environment or `.env`, using the same precedence as existing config (env → `%APPDATA%\xiuxiu\.env` → exe-dir `.env`).
- R2. When `HOTKEY` is unset, the app uses a default of `Ctrl+Alt+Space`.
- R3. An invalid `HOTKEY` string produces a clear startup error naming the offending value and the accepted syntax, surfaced via the existing dialog + log path.
- R4. When registration of the resolved hotkey fails (e.g. already in use), the error message names the hotkey and tells the user to set `HOTKEY` to a free combination.
- R5. The accepted `HOTKEY` syntax (and the AltGr caveat for the default) is documented in `README.md` and `.env.example`.

## Key Technical Decisions

- KTD1. **Store `HOTKEY` as a raw string in config; parse it in `app.rs`, not `config.rs`.** `RawConfig` gains a `hotkey: String` field (defaulted to `Ctrl+Alt+Space`); the actual parse into a `HotKey` happens in `run_inner` where `global-hotkey` is already imported. This keeps `config.rs` free of a `global-hotkey` dependency and keeps its pure map/precedence tests intact, while the parse error surfaces through the single `run()` dialog+log path added in the previous fix.
- KTD2. **Use `global-hotkey`'s native `HotKey: FromStr`, not a hand-rolled parser.** Confirmed for 0.8.0: accelerator syntax — `+`-separated, case-insensitive; modifier tokens `Ctrl`/`Control`, `Alt`/`Option`, `Shift`, `Super`/`Cmd`/`Command` (note: `Win`/`Meta` are **not** valid string tokens — use `Super`); keys like `Space`, `A`/`KeyA`. It is the shared muda/tao parser, not behind a feature flag, and its `HotKeyParseError` is a `thiserror` enum (`Send + Sync + 'static`), so `.parse().context(...)?` works directly.
- KTD3. **Default `Ctrl+Alt+Space`.** Less contended than `Ctrl+Shift+Space` (which is taken on the target machine) and a recognized push-to-talk default. Document the caveat: on keyboard layouts where Right-Alt is AltGr (many non-US layouts), AltGr synthesizes Ctrl+Alt, so those users should override `HOTKEY`. The override path (R1) makes this a non-blocking, documented trade-off rather than a universal default that's perfect for everyone.
- KTD4. **Registration stays fatal but actionable.** A dictation app is unusable without its hotkey, so a registration failure remains a fatal startup error — but the message (already actionable from the prior fix) now also tells the user to set `HOTKEY` to a different combination, and includes the *actual* resolved hotkey string rather than a hardcoded one.

## Implementation Units

### U1. Add the `HOTKEY` config setting

- **Goal:** Read `HOTKEY` from config with the standard precedence, defaulting to `Ctrl+Alt+Space`, exposed as a string on `RawConfig`.
- **Requirements:** R1, R2.
- **Dependencies:** none.
- **Files:** `src/config.rs` (modify — add the key constant, default, `RawConfig` field, `resolve` wiring, **and the `env_map()` key array**; extend the inline `#[cfg(test)]` module).
- **Approach:** Per KTD1. Add `KEY_HOTKEY = "HOTKEY"` and `DEFAULT_HOTKEY = "Ctrl+Alt+Space"`. Add `hotkey: String` to `RawConfig`. In `resolve`, set it from `pick(KEY_HOTKEY)` falling back to `DEFAULT_HOTKEY` (mirror the existing `pick` + default pattern used for `BACKEND`). **Also add `KEY_HOTKEY` to the `env_map()` key array (`[KEY_BACKEND, KEY_GROQ, KEY_MODEL, KEY_HOTKEY]`).** `env_map()` only forwards the keys it lists from the process environment, so without this a real `HOTKEY` environment variable is silently dropped and R1's env precedence breaks (the `.env`-file sources read all keys via dotenvy and would still work, masking the bug). Do not parse or validate the hotkey here — it stays a string (KTD1). `RawConfig::validate` is unchanged (hotkey validity is checked at parse time in U2).
- **Patterns to follow:** the existing `BACKEND` default handling in `config.rs::resolve` (`pick(KEY_BACKEND)` + `DEFAULT_BACKEND`); the existing inline config tests.
- **Test scenarios:**
  - Covers R2. With `HOTKEY` unset in all sources, `resolve` yields `hotkey == "Ctrl+Alt+Space"`.
  - Covers R1. `HOTKEY` set in a source is read through (e.g. `"Alt+Space"`).
  - Covers R1. Precedence holds for `HOTKEY` (env beats appdata beats exedir) — extend the existing precedence test or add a parallel one.
  - Empty/whitespace `HOTKEY` is treated as absent → falls back to the default (consistent with the existing empty-value handling).
- **Verification:** `cargo test` config tests pass; `resolve` returns the default when unset and the override when set. (The `env_map()` change is not cleanly unit-testable without mutating the global process environment; it is covered end-to-end by U2's Windows verification, where `HOTKEY` is set as a real env var.)

### U2. Parse and register the configured hotkey

- **Goal:** Replace the hardcoded hotkey construction with a parse of `config.hotkey`, and make both the parse-error and registration-error messages reference `HOTKEY`.
- **Requirements:** R3, R4.
- **Dependencies:** U1.
- **Files:** `src/app.rs` (modify — the hotkey block in `run_inner`).
- **Approach:** Per KTD2/KTD4. Replace `HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space)` with `let hotkey: HotKey = raw.hotkey.parse().with_context(|| format!("parsing HOTKEY \"{}\" — expected a combination like \"Ctrl+Alt+Space\" (modifiers Ctrl/Alt/Shift/Super + a key, joined by +)", raw.hotkey))?;`. Update the `register(hotkey)` context to interpolate the resolved hotkey string and add the remediation hint, e.g. `format!("registering the global hotkey \"{}\" — it may already be in use by another app (e.g. a Windows IME layout switch or PowerToys); set HOTKEY in your .env to a free combination", raw.hotkey)`. Drop the now-unused `Modifiers`/`Code` imports if nothing else uses them (the manager creation context is unchanged). Both errors flow through the single `run()` surfacing point (dialog + log) added previously.
- **Patterns to follow:** the `.context(...)` usage already in `run_inner`; `anyhow::Context` (`with_context` for the formatted messages).
- **Test scenarios:** `Test expectation: none — this is startup/OS-registration glue with no pure-logic seam (the parser is global-hotkey's, the config default is covered in U1). Verified by running on Windows (below).`
- **Verification (manual, Windows):**
  - With `HOTKEY` unset, the app registers `Ctrl+Alt+Space` and reaches the tray (where `Ctrl+Shift+Space` previously failed).
  - `HOTKEY=Ctrl+Shift+Space` reproduces the conflict, now with a message naming the combo and telling the user to change `HOTKEY`.
  - `HOTKEY=notakey` shows a parse-error dialog naming the value and the expected syntax.
  - A valid free combo (e.g. `HOTKEY=Ctrl+Alt+J`) registers and the hold-to-talk flow works on that combo.

### U3. Document `HOTKEY`

- **Goal:** Document the new setting, its syntax, the default, and the AltGr caveat.
- **Requirements:** R5.
- **Dependencies:** U1, U2 (documents the shipped behavior).
- **Files:** `.env.example` (modify — add a commented `HOTKEY` entry with syntax + examples), `README.md` (modify — add `HOTKEY` to the config table and a short note on syntax, the `Ctrl+Alt+Space` default, the AltGr caveat, and "if the hotkey is already in use, set `HOTKEY` to a free combo").
- **Approach:** Keep it concise and consistent with the existing config table. State the accepted modifier tokens (`Ctrl`/`Control`, `Alt`/`Option`, `Shift`, `Super`/`Cmd`/`Command`) and that keys use names like `Space`, letters as `A`/`KeyA`; give 1-2 examples (`Ctrl+Alt+Space`, `Ctrl+Alt+J`). Note `Win`/`Meta` are not accepted (use `Super`). **Document the AltGr caveat** (R5/KTD3): on keyboard layouts where Right-Alt is AltGr (many non-US layouts), AltGr synthesizes Ctrl+Alt, so those users should override `HOTKEY` to a combo without Alt. Add commented example values to `.env.example` (e.g. `# HOTKEY=Ctrl+Alt+J`).
- **Test scenarios:** `Test expectation: none — documentation only.`
- **Verification:** A reader can set a working `HOTKEY` from the README/`.env.example` without consulting the crate docs.

## Verification & Notes

- The end-to-end confirmation is the user's Windows `cargo run`: default `Ctrl+Alt+Space` should register where `Ctrl+Shift+Space` failed; the three `HOTKEY` cases above (valid default, intentional conflict, invalid string) each produce the expected outcome.
- This builds directly on the prior startup-error-surfacing fix — the parse and registration errors rely on that single dialog+log path, so no new surfacing logic is needed.
- Source + docs only; no dependency changes (`global-hotkey` is already a dependency).

## Scope Boundaries

- In scope: a launch-time `HOTKEY` config string, parsed and registered, with actionable errors and docs.
- Deferred to follow-up work: a tray-menu hotkey picker or live re-binding without restart (config-at-launch only for v1, matching the app's existing config model); remembering/rotating among multiple hotkeys.
- Out of scope: changing the capture/transcription/injection flow, or the input model itself (push-to-talk on a held combo stays as-is).
