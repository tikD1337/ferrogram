# Setup on a new machine

Everything needed to continue development is in the repository, except for three things
intentionally kept out of git: API credentials, the TDLib build and build directories.

## 1. Tools

| What | How to install | Why |
|---|---|---|
| Git | `winget install Git.Git` | Source code |
| rustup | `winget install Rustlang.Rustup` | Rust; the required version (1.100.0) is installed automatically from `rust-toolchain.toml` |
| MSVC Build Tools 2022 or later | `winget install Microsoft.VisualStudio.BuildTools`, select “Desktop development with C++” | Linker for Rust, compiler and CMake for TDLib |
| GitHub CLI | `winget install GitHub.cli`, then `gh auth login` | Cloning, pull requests and releases |

## 2. Source code

```powershell
gh repo clone tikD1337/veygram
```

## 3. API credentials

Copy `.env.example` to `.env` and fill in `TG_API_ID` and `TG_API_HASH`. You can
find them at https://my.telegram.org or transfer them manually from the previous machine —
never through git or in plain text through a messenger.

## 4. Build verification

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## 5. Feed prototype

```powershell
cd spikes\slint-feed
cargo build --release
.\target\release\slint-feed-spike.exe --mode custom --start-bottom
.\bench.ps1 -Backends winit-software -ExtraArgs '--mode','custom'
```

The first Slint build takes several minutes.

## 6. TDLib

Built from official source at a pinned commit, outside the repository:

```powershell
.\scripts\build-tdlib.ps1 -Root <tdlib directory> -Vcpkg <vcpkg directory>
```

By default both live next to the repository directory (`..\tdlib`, `..\vcpkg`).
Point `TDLIB_DIR` at `<Root>\install` when building the client.
The build takes tens of minutes and occupies several gigabytes. The result is
`<Root>\install\bin\tdjson.dll` and the DLLs alongside it.

At run time the client loads `tdjson.dll` from its own folder when it is there (a release),
otherwise from `TDLIB_DIR\bin` recorded at build time (a development build).

## 6a. A release package

`scripts\package-release.ps1` builds the portable zip that GitHub Releases offer: `veygram.exe`,
`tdjson.dll` with OpenSSL and zlib, the Visual C++ runtime DLLs and the license texts.

```powershell
.\scripts\package-release.ps1 -TdlibBin <Root>\install\bin -CrtDir <MSVC redist>\x64\Microsoft.VC145.CRT -LicenseDir <vcpkg>\installed\x64-windows\share
```

It reads `TG_API_ID` and `TG_API_HASH` from `.env` and builds them into the executable
(`VEYGRAM_BUILD_API_ID`, `VEYGRAM_BUILD_API_HASH`), as every distributed Telegram client carries its
own application credentials. It never prints them and builds into its own `target-release`
directory, so development builds do not contain them. The zip and its SHA-256 land in
`target-release\dist`.

## 7. What to read to get up to speed

1. [ROADMAP.md](ROADMAP.md) — what is done and what comes next.
2. [ARCHITECTURE.md](ARCHITECTURE.md) — accepted decisions and their rationale.
3. [BENCHMARKS.md](BENCHMARKS.md) — all measurements.
4. [SECURITY.md](SECURITY.md) — session protection, process hardening and the release's trust model.
5. [../CLAUDE.md](../CLAUDE.md) — project rules for AI coding assistants; machine-specific notes go
   into `CLAUDE.local.md`, which is not committed.

## What does not transfer

- **Assistant memory and code-graph indexes** are tied to the machine. Everything essential is in
  `docs/` and `CLAUDE.md`; on a new machine, reindexing the project is enough.
- **The Telegram session.** The protection scheme ties it to the computer, so you need
  to sign in again on a new machine.
- **Measurements** depend on the hardware: numbers from different machines cannot be compared.

## 8. TDLib probe

Measures what TDLib alone costs. Build it once, then run it **from your own terminal**:
the first start asks for your phone number, login code and two-step password.

```powershell
cd spikes\tdlib-probe
cargo build --release
cd ..\..
.\spikes\tdlib-probe\run.ps1
```

If the login code does not arrive (this happens with newly registered `api_id` values), sign in
by QR code instead: run `.\spikes\tdlib-probe\run.ps1 -Qr` and scan the code with Telegram on
your phone (Settings > Devices > Link Desktop Device).

When it prints `READY`, measure it from a second terminal:

```powershell
.\scripts\measure.ps1 -Name tdlib-probe -DurationSec 600 -Csv .\bench\results\<date>-tdlib-idle.csv
```

The session is stored in `tdlib-data/` (excluded from git), encrypted with a random key
that is protected by DPAPI, so later starts do not ask to sign in again and the directory
is useless on another machine or account. Delete `tdlib-data/` to sign out locally.
