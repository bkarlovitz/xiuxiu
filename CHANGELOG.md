# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project aims
to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Initial implementation of Xiuxiu:
  - Push-to-talk capture (`Ctrl+Shift+Space`) with build-on-press microphone
    handling (mic open only while held).
  - Two transcription backends — local whisper.cpp (`whisper-rs`) and Groq —
    switchable at runtime from the tray menu, with local→Groq fallback.
  - Unicode text injection into the focused window, with control-character
    sanitization for terminal safety.
  - System-tray UI with idle/recording icon states; no window, no console.
  - `.env` configuration (`%APPDATA%` or exe-dir) with secret redaction.
  - File logging + panic hook with a crash dialog; transcripts and the API key
    are never logged.

[Unreleased]: https://github.com/bkarlovitz/xiuxiu/commits/main
