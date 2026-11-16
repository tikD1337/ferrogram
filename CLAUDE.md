# Project rules

A native Telegram client for Windows 10/11 x64, written in Rust. The main success criterion is
measurably lower RAM and CPU usage than the official Telegram Desktop, without
sacrificing functionality, correctness or security.

## Starting a session

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/ROADMAP.md](docs/ROADMAP.md),
[docs/SECURITY.md](docs/SECURITY.md), [docs/BENCHMARKS.md](docs/BENCHMARKS.md).
Personal working rules, if any, are in `CLAUDE.local.md` (not committed).

## Decisions

- Protocol — TDLib. GUI — Slint. Both choices are validated by stage 0 measurements.
- The UI and engine communicate only through types from `tg-core`.
- Only `tg-telegram` works with TDLib; only `tg-ui` works with the GUI framework.

## Mandatory rules

- Optimize only after measuring. Do not give numbers without measurements; label estimates as estimates.
- Do not claim that code builds or a feature works unless this has been verified.
- No polling or busy-waiting. Heavy work stays off the UI thread.
- Caches are bounded in bytes. The entire conversation history is never loaded into memory.
- `unsafe` is allowed only in FFI crates, with a justification for each block. `.unwrap()` is forbidden outside tests.
- Do not write custom cryptography. Do not log secrets.
- Obtain approval for changes to architecture, security and anything that deletes user data.
- After changes: `cargo fmt`, `cargo clippy`, `cargo check`, `cargo test`.
- After significant changes, update the documents in `docs/`.

## How to work

- New machine: [docs/SETUP.md](docs/SETUP.md).

## Environment

- Machine-specific paths (toolchain, TDLib, data) are in `CLAUDE.local.md`, which is not committed.
- Rust is pinned in `rust-toolchain.toml`. TDLib is built outside the repository; `TDLIB_DIR` points to its install directory.
- Disposable prototypes live in `spikes/` outside the workspace; their code is not carried over into the product.
- Telegram application credentials are in `.env` at the root (`TG_API_ID`, `TG_API_HASH`). The file is excluded from git.
  Do not copy the values into code, documents, logs, reports or project memory.
- Cargo warns about unused dependencies: add a dependency together with the code that uses it.
- PowerShell scripts must be ASCII only: PowerShell 5.1 reads UTF-8 without a BOM as cp1251.
- The crate name `core` is taken by the standard library, so packages are named `tg-*`.

## Language

- Everything publicly visible on GitHub is in English: README, docs, code comments, repository description, commit messages,
  PR titles and descriptions, crate descriptions in `Cargo.toml`.
