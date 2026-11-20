# Telegram session protection

Status: **accepted 2026-11-16, partly implemented.** Done: the TPM + DPAPI key chain, the key on a
locked page and zeroed, process hardening (2026-11-17) and a recovery screen when the key cannot be
unlocked (2026-11-17), and the trust model of the public release package (2026-11-18). Not done
yet: the master password, Windows Hello, automatic locking, code signing and dependency audits in CI.
The order is in [ROADMAP.md](ROADMAP.md), "Security work".

## Goal

Copying the application folder must not allow the session to be restored on another
computer. Beyond that, make theft harder for malware running on the same machine.

## What exactly we protect

The MTProto authorization key. The Telegram server does not bind it to a device: whoever obtains
the key obtains the session. This is exactly how stealers target Telegram Desktop —
by copying the `tdata` folder.

TDLib stores authorization keys in its database and encrypts it with a key supplied by
the application (`database_encryption_key`). The task therefore comes down to one thing: **store
this key securely.** We do not write custom cryptography.

Verify with a test at stage 2 that encryption covers both the `td.binlog` log and
the SQLite databases, and that the database does not open with an incorrect key. TDLib does not encrypt
downloaded media files — this is a known limitation.

## Threat model

| # | Attacker | Can we protect against this |
|---|---|---|
| T1 | Copied the files and took them to their own machine | **Yes, completely** |
| T2 | Malware under the user's account, client closed or locked | **Yes — only with a master password or Windows Hello** |
| T3 | Malware under the user's account, client running and unlocked | **Partially.** We make it harder, but offer no guarantee |
| T4 | Malware with administrator privileges or in the kernel | **No** |

An honest assessment of T3 and T4: while the client is running, keys are in process memory in plain text —
otherwise it could not communicate with the server. A program with sufficient privileges can read them.
A keylogger can also capture the master password. This is a limitation of any program on
a conventional OS, rather than an architectural flaw.

## Architecture

```
random database key (DEK, 256 bits)
        │ wrapped by a chain:
        ├─ 1. TPM key (non-exportable)             ← bound to the hardware
        ├─ 2. DPAPI                                ← bound to the user account
        └─ 3. master password (Argon2id), optional ← secret kept off the computer
```

The layers are nested: obtaining the DEK requires passing through every enabled layer.

**1. TPM.** A non-exportable key in Microsoft Platform Crypto Provider (CNG, `NCrypt*`).
The private part never leaves the chip, so the files are useless on another computer.
Addresses T1. If no TPM is available, this layer is skipped and the user sees a warning.

**2. DPAPI** (`CryptProtectData`). Binding to the Windows account. Complements TPM and
provides the only binding where no TPM is available. DPAPI alone is insufficient: malware
running as the same user can invoke decryption itself — this is how Chrome cookies were stolen.

**3. Master password.** The only way to address T2: the secret is not on the machine. The key is
derived through Argon2id; wrapping uses AEAD from audited RustCrypto implementations.
The cost is entering a password at startup. Disabled by default, enabled in settings.

**Windows Hello as an alternative to a password** — fingerprint, face or PIN instead of typing
a password; the TPM key is released only after a user gesture. More convenient, but requires
research: the standard API provides signing, rather than decryption. Investigate at stage 4.

### In memory

- The DEK lives on a private page of its own (`VirtualAlloc`), locked into RAM with `VirtualLock`
  so it never reaches the page file, and is zeroed before the page is unlocked and released. Our
  copy is dropped right after it is passed to TDLib. Locking is best effort: if Windows refuses it
  (working-set quota), the key still works without it.
- We have no control over keys inside TDLib: this is memory owned by a C++ library.
- Automatic locking: an idle timer shuts down TDLib, removing keys from memory.
  The tradeoff is no notifications while locked. Two modes to choose from:
  lock only the UI or lock fully.

### Process hardening (against T3), implemented 2026-11-17

`tg_platform::harden_process`, called first thing in `main`:

- **Process DACL.** Programs running as the same user may query, wait for and end the process
  (Task Manager keeps working), but not read or write its memory, create threads in it, duplicate
  its handles or change its permissions. An OWNER RIGHTS entry removes the owner's implicit right
  to rewrite the DACL. SYSTEM keeps full access. Stops typical stealers without administrator
  rights; an administrator with SeDebugPrivilege still gets in.
- **DLL search order**, as Telegram Desktop does: libraries loaded by name come only from System32
  (`SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)`). TDLib is loaded by absolute path
  with its own directory for its dependencies: `tdjson.dll` next to `veygram.exe` in a release,
  otherwise the TDLib installation the build was made against. Never from the working directory.
- **Image load policy:** no images from network shares or low-integrity locations.
- Not enabled: signed-only images, because TDLib and its OpenSSL DLLs are not Microsoft-signed and
  would be refused. Prohibiting dynamic code and disabling extension points were not tried: both are
  known to conflict with graphics drivers and legacy input methods, and need a measured trial first.

Verified on 2026-11-17: an automated test opens its own hardened process by PID and gets
access-denied for memory reads and DACL changes; the demo, video playback (child window present),
voice playback (position advancing) and the live client with TDLib all work with it. The cost in
RAM and CPU has not been measured separately; it is a handful of system calls at startup.

### When the key cannot be unlocked

If the key file exists but the TPM or DPAPI refuses it (or it is not a format this build reads),
the engine does not start and the client explains the situation with two choices: **Try again**,
or **Start fresh**, which renames the data directory to `<name>-backup-<UTC date and time>` next
to it and starts with a new key. Nothing is deleted. Telegram Desktop in the same situation
silently starts from scratch and overwrites its local data. I/O and directory errors never offer
a fresh start.

This raises the cost of an attack, but is not a security boundary.

### The release package (2026-11-18)

The public release is a zip from `scripts/package-release.ps1`: `veygram.exe`, `tdjson.dll`,
OpenSSL, zlib, the Visual C++ runtime DLLs and the license texts, unpacked into one folder.

- **Application credentials are built in.** Every Telegram client that is distributed carries its
  `api_id` and `api_hash` (Telegram Desktop compiles them in as `TDESKTOP_API_ID`/`TDESKTOP_API_HASH`).
  They identify the application to Telegram, not the user, and give no access to any account; anyone
  can extract them from the executable. The risk is to the application: someone could use them in
  another program, and Telegram could limit them. They never enter the repository or a log; the
  build reads them from `.env` into build-time variables. Veygram uses its own credentials, never
  Telegram Desktop's: Telegram asks every third-party client to register its own (my.telegram.org;
  Telegram Desktop's sources refuse to build without them), and presenting another client's pair
  would impersonate that client. An environment variable or `.env` still overrides the built-in pair.
- **Libraries beside the executable.** `tdjson.dll` is loaded by its absolute path in the
  executable's folder, its dependencies from that folder and System32, never from the working
  directory. Whoever can write into that folder can also replace `veygram.exe`, so this adds no new
  attack: unpack the release into a folder only your account can write to, not a shared one.
- **No code signing yet.** SmartScreen warns on first start. The release publishes the zip's SHA-256
  next to it; signing is open.
- The Visual C++ runtime is copied app-locally, as Microsoft allows, so it is not updated by Windows
  Update; a new release brings a new copy.

### What we do not do

- **Chrome-style application binding** (a service with SYSTEM privileges): requires
  an installer with administrator privileges, substantially complicates the product and has already been bypassed in practice.
- **Anti-debugging and obfuscation:** not protection.

### Other measures

- Codes, passwords, keys and phone numbers never enter logs.
- A pasted clipboard image is written unencrypted to the user's temporary folder (per-user ACL)
  only while it is previewed or uploading, and deleted afterwards. Paths and captions of sent
  files are not logged, and error messages do not echo them.
- Planned: a list of active sessions and a recommendation to enable two-step verification in
  the UI, so a stolen session can be ended from another device.
- Planned: dependency checks with `cargo audit` and `cargo deny` in CI.

## Decisions for you

1. Master password: disabled by default (proposed) or mandatory?
2. Automatic locking: UI only (notifications work) or full (more secure)?
3. No TPM: use DPAPI alone with a warning (proposed) or require a master password?

## Sources

- [Raymond Chen on ways to bind data to a user and machine](https://devblogs.microsoft.com/oldnewthing/20240327-00/?p=109580)
- [CyberArk: bypassing Chrome's app-bound cookie encryption](https://www.cyberark.com/resources/threat-research-blog/c4-bomb-blowing-up-chromes-appbound-cookie-encryption)
- [Synacktiv: Windows Hello keys and TPM](https://synacktiv.com/publications/whfb-and-entra-id-say-hello-to-your-new-cache-flow)

## Decisions (2026-11-16)

The owner's requirement: a copied session directory must be useless to an attacker because
it is encrypted, and the development machine has a TPM. Decided on that basis:

1. **TPM + DPAPI is the default and is always on.** The database key is wrapped by a
   non-exportable TPM key and then by DPAPI. This is the layer that makes copied files
   useless on another computer.
2. **Master password: optional, off by default.** It is the only thing that also stops
   malware running under the same Windows account while the client is closed, so it is
   offered prominently in settings rather than forced on every start.
3. **Auto-lock: interface-only by default** (notifications keep working), with full lock
   available as a setting.
4. **No TPM:** fall back to DPAPI alone and show a warning.

Items 2–4 are the defaults proposed earlier; the owner answered the question with the
requirement above rather than item by item, so they can still be changed.

## TPM check on the primary machine (2026-11-16)

Measured with `spikes/tpm-probe` (Microsoft Platform Crypto Provider, RSA-2048, persistent key):

- Key creation: about 1.1 s, once.
- Export policy is 0 and `NCryptExportKey` of the private key is refused (`0x8009000A`).
- OAEP SHA-256: 32 bytes wrap to 256 bytes, round trip correct, unwrap 39 ms. OAEP SHA-1 also
  works (16 ms). The product uses SHA-256.
- The key is found again by name after the handle is closed. The probe deletes its key on exit.

Not checked: behaviour after a TPM reset or a firmware update, and machines without a TPM.

## Reporting a vulnerability

Please do not open a public issue for a security problem. Use GitHub's private vulnerability
reporting for this repository ("Security" tab, "Report a vulnerability"), with steps to
reproduce and the affected version. Never include your session data, `.env` or the
`database-key.bin` file in a report.
