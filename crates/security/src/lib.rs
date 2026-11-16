//! Windows database-key protection: RSA-2048 OAEP SHA-256 in the TPM, then
//! current-user DPAPI. No TDLib, UI, credentials or logging belongs here.
//!
//! All entry points that access storage or cryptography are blocking and must run
//! off the UI thread. Callers must show a security warning when `tpm_protected` is
//! false, including when reopening an existing DPAPI-only file.

use ffi::Zeroing;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

// Unsafe code is confined to the Windows API boundary.
#[allow(unsafe_code)]
mod ffi;

#[cfg(test)]
mod tests;

const MAGIC: &[u8; 8] = b"TGRUSTDK";
const VERSION: u8 = 1;
const MODE_DPAPI: u8 = 0;
const MODE_TPM: u8 = 1;
const HEADER_LEN: usize = MAGIC.len() + 2;
// Bounds every on-disk read and native DPAPI buffer copied into Rust.
const MAX_FILE_SIZE: usize = 16 * 1024;
/// The file contains only the versioned, protected database key.
pub const KEY_FILE_NAME: &str = "database-key.bin";

/// A fixed-length secret. No Clone, Copy, Display, serialization or public constructor.
/// Heap storage is zeroed on drop, including early error returns.
pub struct DatabaseKey(Box<Zeroing<[u8; 32]>>);

impl DatabaseKey {
    fn empty() -> Self {
        Self(Box::new(Zeroing::new([0; 32])))
    }

    fn from_slice(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 32 {
            return Err(Error::InvalidFormat);
        }
        let mut key = Self::empty();
        key.0.as_mut().copy_from_slice(bytes);
        Ok(key)
    }

    /// Borrow only long enough to supply TDLib's key parameter, then drop this owner.
    /// Copies made by the caller or TDLib are outside this type's control.
    #[must_use]
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for DatabaseKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DatabaseKey(<redacted>)")
    }
}

/// Result consumed by tg-app; false requires the TPM-unavailable warning event.
#[derive(Debug)]
pub struct KeyProtection {
    pub key: DatabaseKey,
    pub tpm_protected: bool,
}

/// Sanitized errors contain neither paths, plaintext nor native diagnostic strings.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Io(io::ErrorKind),
    Random,
    InvalidFormat,
    UnsupportedVersion(u8),
    UnsafeDataDirectory,
    MissingTpmKey,
    Native { operation: &'static str, code: u32 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "Key storage I/O failed ({kind:?})"),
            Self::Random => f.write_str("Secure random generation failed"),
            Self::InvalidFormat => f.write_str("Invalid protected-key format"),
            Self::UnsupportedVersion(v) => write!(f, "Unsupported protected-key version {v}"),
            Self::UnsafeDataDirectory => f.write_str("Invalid application data directory"),
            Self::MissingTpmKey => f.write_str("The persistent TPM key is missing"),
            Self::Native { operation, code } => write!(f, "{operation} failed (0x{code:08X})"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

/// Select `%LOCALAPPDATA%\tg-rust` or the absolute `TG_DATA_DIR` override.
/// Does not read or migrate any prototype data. Relative and probe paths are rejected.
pub fn data_dir() -> Result<PathBuf, Error> {
    select_data_dir(
        std::env::var_os("TG_DATA_DIR"),
        std::env::var_os("LOCALAPPDATA"),
    )
}

fn select_data_dir(
    override_dir: Option<std::ffi::OsString>,
    local_app_data: Option<std::ffi::OsString>,
) -> Result<PathBuf, Error> {
    let path = if let Some(path) = override_dir {
        PathBuf::from(path)
    } else {
        PathBuf::from(local_app_data.ok_or(Error::UnsafeDataDirectory)?).join("tg-rust")
    };
    validate_directory(&path)?;
    Ok(path)
}

fn validate_directory(path: &Path) -> Result<(), Error> {
    if !path.is_absolute()
        || path.components().any(|part| {
            part.as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case("tdlib-data")
        })
    {
        return Err(Error::UnsafeDataDirectory);
    }
    Ok(())
}

/// Load the existing key, or create it once if the file does not exist.
/// Unknown versions, decryption errors and TPM failures never reset an existing file.
/// Existing TPM files require the original persistent TPM key; no downgrade is attempted.
pub fn load_or_create(directory: &Path) -> Result<KeyProtection, Error> {
    load_or_create_with(directory, &ffi::Native)
}

trait Protection {
    fn tpm_wrap(&self, key: &[u8; 32]) -> Result<Option<Vec<u8>>, Error>;
    fn tpm_unwrap(&self, wrapped: &[u8]) -> Result<DatabaseKey, Error>;
    fn protect(&self, input: &[u8]) -> Result<Vec<u8>, Error>;
    fn unprotect(&self, input: &[u8]) -> Result<Zeroing<Vec<u8>>, Error>;
}

fn load_or_create_with(directory: &Path, crypto: &impl Protection) -> Result<KeyProtection, Error> {
    validate_directory(directory)?;
    fs::create_dir_all(directory)?;
    // Also reject existing junctions/symlinks resolving into the prototype directory.
    let directory = directory.canonicalize()?;
    validate_directory(&directory)?;
    let path = directory.join(KEY_FILE_NAME);
    match read_key(&path, crypto) {
        Err(Error::Io(io::ErrorKind::NotFound)) => {}
        result => return result,
    }
    let mut key = DatabaseKey::empty();
    getrandom::fill(&mut key.0[..]).map_err(|_| Error::Random)?;
    let wrapped = crypto.tpm_wrap(key.expose())?;
    let mode = if wrapped.is_some() {
        MODE_TPM
    } else {
        MODE_DPAPI
    };
    // At most HEADER_LEN + 256 bytes. Allocate once so a plaintext key is never
    // left behind by Vec reallocation. Zeroing clears all initialized bytes.
    let mut inner = Zeroing::new(Vec::with_capacity(HEADER_LEN + 256));
    inner.extend_from_slice(MAGIC);
    inner.extend_from_slice(&[VERSION, mode]);
    inner.extend_from_slice(wrapped.as_deref().unwrap_or(key.expose()));
    let protected = crypto.protect(&inner)?;
    if protected.is_empty() || protected.len() > MAX_FILE_SIZE - HEADER_LEN {
        return Err(Error::InvalidFormat);
    }
    // At most MAX_FILE_SIZE bytes, ciphertext only.
    let mut file = Vec::with_capacity(HEADER_LEN + protected.len());
    file.extend_from_slice(&inner[..HEADER_LEN]);
    file.extend_from_slice(&protected);
    match publish(&path, &file) {
        Ok(()) => Ok(KeyProtection {
            key,
            tpm_protected: mode == MODE_TPM,
        }),
        // Another process won creation. Use its key, never overwrite it.
        Err(Error::Io(io::ErrorKind::AlreadyExists)) => read_key(&path, crypto),
        Err(error) => Err(error),
    }
}

fn read_key(path: &Path, crypto: &impl Protection) -> Result<KeyProtection, Error> {
    let file = File::open(path)?;
    // A malicious/oversized file cannot cause an unbounded allocation.
    let mut bytes = Vec::with_capacity(MAX_FILE_SIZE + 1);
    file.take((MAX_FILE_SIZE + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_SIZE || bytes.len() <= HEADER_LEN || &bytes[..MAGIC.len()] != MAGIC {
        return Err(Error::InvalidFormat);
    }
    if bytes[MAGIC.len()] != VERSION {
        return Err(Error::UnsupportedVersion(bytes[MAGIC.len()]));
    }
    let mode = bytes[HEADER_LEN - 1];
    if ![MODE_DPAPI, MODE_TPM].contains(&mode) {
        return Err(Error::InvalidFormat);
    }
    let inner = crypto.unprotect(&bytes[HEADER_LEN..])?;
    // The authenticated DPAPI plaintext binds the otherwise unauthenticated header.
    if inner.len() < HEADER_LEN || inner[..HEADER_LEN] != bytes[..HEADER_LEN] {
        return Err(Error::InvalidFormat);
    }
    let payload = &inner[HEADER_LEN..];
    let key = if mode == MODE_TPM {
        if payload.len() != 256 {
            return Err(Error::InvalidFormat);
        }
        crypto.tpm_unwrap(payload)?
    } else {
        DatabaseKey::from_slice(payload)?
    };
    Ok(KeyProtection {
        key,
        tpm_protected: mode == MODE_TPM,
    })
}

struct PendingFile(PathBuf);

impl Drop for PendingFile {
    fn drop(&mut self) {
        // Only our temporary file, never the destination, is removed on failure.
        let _ = fs::remove_file(&self.0);
    }
}

fn publish(destination: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut suffix = [0; 16];
    getrandom::fill(&mut suffix).map_err(|_| Error::Random)?;
    let temporary = destination.with_file_name(format!(
        ".database-key-{:032x}.tmp",
        u128::from_le_bytes(suffix)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let pending = PendingFile(temporary);
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    ffi::publish(&pending.0, destination)?;
    Ok(())
}
