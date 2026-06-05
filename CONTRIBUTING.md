# Contributing to Xiuxiu

Thanks for your interest in Xiuxiu!

## Pull requests are not accepted

This is a personal project maintained solely by the author. **`main` is not open
to outside code, and pull requests will be closed unmerged.** Please don't spend
effort on a PR — it won't be merged.

## What *is* welcome: issues

The best way to help is to **[open an issue](../../issues)**:

- **Bug reports** — what happened, what you expected, and (importantly) the
  contents of `xiuxiu.log` from next to the executable, your Windows version, and
  which backend (`local`/`groq`) was active.
- **Feature ideas** — what you want and why.
- **Questions** — about building, configuring, or using Xiuxiu.

## Want to build on it?

Xiuxiu is MIT-licensed — **fork it freely** and take it wherever you like. You
don't need permission, and you're not expected to send changes back.

## For the maintainer

Local workflow (on Windows — see the [README](README.md#install--build) for the
toolchain):

```powershell
cargo fmt --all
cargo clippy --all-targets
cargo test
cargo build --release
```

Conventions: pure logic lives in inline `#[cfg(test)]` modules with tests; the
`!Send` audio stream stays on its dedicated thread; secrets and transcripts are
never logged. Design rationale is in
[`docs/plans/`](docs/plans/).
