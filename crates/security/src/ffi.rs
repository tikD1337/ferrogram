//! The only unsafe boundary: CNG handles, DPAPI allocations and atomic file publication.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    LocalFree, NTE_BAD_KEYSET, NTE_DEVICE_NOT_READY, NTE_EXISTS, NTE_NOT_FOUND, NTE_NOT_SUPPORTED,
    TBS_E_TPM_NOT_FOUND, TPM_E_DISABLED,
};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_OAEP_PADDING_INFO, BCRYPT_RSA_ALGORITHM, BCRYPT_SHA256_ALGORITHM, CRYPT_INTEGER_BLOB,
    CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData, MS_PLATFORM_CRYPTO_PROVIDER,
    NCRYPT_ALLOW_DECRYPT_FLAG, NCRYPT_EXPORT_POLICY_PROPERTY, NCRYPT_KEY_USAGE_PROPERTY,
    NCRYPT_LENGTH_PROPERTY, NCRYPT_PAD_OAEP_FLAG, NCRYPT_SILENT_FLAG, NCryptCreatePersistedKey,
    NCryptDecrypt, NCryptEncrypt, NCryptFinalizeKey, NCryptFreeObject, NCryptGetProperty,
    NCryptOpenKey, NCryptOpenStorageProvider, NCryptSetProperty,
};
use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
use windows_sys::core::{PCWSTR, w};

use super::{DatabaseKey, Error, MAX_FILE_SIZE, Path, Protection, io};

/// Owns initialized bytes and clears them on every drop path. Kept at the FFI
/// boundary because Windows' SecureZeroMemory is an inline volatile-store routine,
/// not an exported function available through windows-sys.
pub(super) struct Zeroing<T: AsMut<[u8]>>(T);

impl<T: AsMut<[u8]>> Zeroing<T> {
    pub(super) fn new(value: T) -> Self {
        Self(value)
    }
}

impl<T: AsMut<[u8]>> std::ops::Deref for Zeroing<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: AsMut<[u8]>> std::ops::DerefMut for Zeroing<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T: AsMut<[u8]>> Drop for Zeroing<T> {
    fn drop(&mut self) {
        wipe(self.0.as_mut());
    }
}

/// The 32 key bytes on a private page of their own, locked into RAM so the key never reaches the
/// page file. The page is zeroed, unlocked and released on drop. Locking is best effort: Windows
/// may refuse it when the working-set quota is exhausted, and the key still works then.
pub(super) struct LockedKey {
    page: std::ptr::NonNull<[u8; 32]>,
    locked: bool,
}

// SAFETY: LockedKey uniquely owns its page; nothing else aliases it.
unsafe impl Send for LockedKey {}
unsafe impl Sync for LockedKey {}

impl LockedKey {
    pub(super) fn new() -> Result<Self, Error> {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualLock,
        };
        // SAFETY: a fresh read-write allocation of one page; a null result is handled. The OS
        // zero-fills committed pages, so the array is initialized.
        let raw = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                4096,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        let page = std::ptr::NonNull::new(raw.cast::<[u8; 32]>()).ok_or(Error::Native {
            operation: "key page allocation",
            code: 0,
        })?;
        // SAFETY: the address and size describe the page committed above.
        let locked = unsafe { VirtualLock(raw, 4096) } != 0;
        Ok(Self { page, locked })
    }
    pub(super) fn bytes(&self) -> &[u8; 32] {
        // SAFETY: the page is live and owned for &self's lifetime and was zero-initialized.
        unsafe { self.page.as_ref() }
    }
    pub(super) fn bytes_mut(&mut self) -> &mut [u8; 32] {
        // SAFETY: &mut self guarantees exclusive access to the owned, initialized page.
        unsafe { self.page.as_mut() }
    }
    #[cfg(test)]
    pub(super) fn is_locked(&self) -> bool {
        self.locked
    }
}

impl Drop for LockedKey {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Memory::{MEM_RELEASE, VirtualFree, VirtualUnlock};
        // Zero while the page is still locked, then unlock and release it.
        wipe(self.bytes_mut());
        let raw = self.page.as_ptr().cast::<core::ffi::c_void>();
        // SAFETY: the page was allocated (and possibly locked) by `new`, is released exactly once
        // here, and no reference to it outlives this call.
        unsafe {
            if self.locked {
                VirtualUnlock(raw, 4096);
            }
            VirtualFree(raw, 0, MEM_RELEASE);
        }
    }
}

fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        // SAFETY: each pointer comes from an exclusive reference to an initialized
        // byte. Volatile stores cannot be eliminated as dead writes before free.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

// Storage identifier from before the rename to Ferrogram: changing it would orphan existing
// database keys. Application constant, deliberately different from tg-rust-tpm-probe. Never
// delete or overwrite this key: it may protect several application data directories.
const TPM_KEY_NAME: PCWSTR = w!("tg-rust-database-wrap-v1");
// Domain separation, not a secret. Current-user DPAPI plus this entropy is mandatory.
const ENTROPY: &[u8] = b"tg-rust/database-key/dpapi/v1";

pub(super) struct Native;

impl Protection for Native {
    fn tpm_wrap(&self, key: &[u8; 32]) -> Result<Option<Vec<u8>>, Error> {
        let provider = match open_provider() {
            Ok(provider) => provider,
            Err(Error::Native { code, .. }) if unavailable(code as i32) => return Ok(None),
            Err(error) => return Err(error),
        };
        let handle = match open_or_create_key(&provider) {
            Ok(handle) => handle,
            Err(Error::Native {
                operation: "TPM open",
                code,
            }) if unavailable(code as i32) => {
                return Ok(None);
            }
            Err(Error::Native {
                operation: "TPM create",
                code,
            }) if unavailable(code as i32) => {
                return Ok(None);
            }
            Err(Error::Native {
                operation: "TPM finalize",
                code,
            }) if unavailable(code as i32) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        validate_key(&handle)?;
        let wrapped = crypt(&handle, key, false)?;
        if wrapped.len() != 256 {
            return Err(Error::InvalidFormat);
        }
        Ok(Some(wrapped.to_vec()))
    }

    fn tpm_unwrap(&self, wrapped: &[u8]) -> Result<DatabaseKey, Error> {
        let provider = open_provider()?;
        // Loading a file must never create a replacement TPM key or downgrade.
        let handle = open_key(&provider)?;
        validate_key(&handle)?;
        let plaintext = crypt(&handle, wrapped, true)?;
        DatabaseKey::from_slice(&plaintext)
    }

    fn protect(&self, input: &[u8]) -> Result<Vec<u8>, Error> {
        protect(input)
    }

    fn unprotect(&self, input: &[u8]) -> Result<Zeroing<Vec<u8>>, Error> {
        unprotect(input)
    }
}

// Only absence/disabled/unsupported hardware permits fallback during creation.
// Permissions, UI requirements, resource failures and cryptographic failures are errors.
fn unavailable(status: i32) -> bool {
    matches!(
        status,
        NTE_DEVICE_NOT_READY | NTE_NOT_SUPPORTED | TBS_E_TPM_NOT_FOUND | TPM_E_DISABLED
    )
}

struct Handle(usize);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: this owner holds a successfully acquired CNG handle, freed once.
        unsafe { NCryptFreeObject(self.0) };
    }
}

fn status(operation: &'static str, code: i32) -> Result<(), Error> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::Native {
            operation,
            code: code as u32,
        })
    }
}

fn open_provider() -> Result<Handle, Error> {
    let mut raw = 0;
    // SAFETY: live out pointer and a static NUL-terminated provider name.
    status("TPM provider", unsafe {
        NCryptOpenStorageProvider(&mut raw, MS_PLATFORM_CRYPTO_PROVIDER, 0)
    })?;
    Ok(Handle(raw))
}

fn open_key(provider: &Handle) -> Result<Handle, Error> {
    let mut raw = 0;
    // SAFETY: provider is live, out pointer is writable, key name is static.
    let result =
        unsafe { NCryptOpenKey(provider.0, &mut raw, TPM_KEY_NAME, 0, NCRYPT_SILENT_FLAG) };
    if matches!(result, NTE_BAD_KEYSET | NTE_NOT_FOUND) {
        return Err(Error::MissingTpmKey);
    }
    status("TPM open", result)?;
    Ok(Handle(raw))
}

fn open_or_create_key(provider: &Handle) -> Result<Handle, Error> {
    match open_key(provider) {
        Err(Error::MissingTpmKey) => {}
        result => return result,
    }
    let mut raw = 0;
    // SAFETY: live provider and out pointer; fixed static names. No overwrite flag.
    let result = unsafe {
        NCryptCreatePersistedKey(
            provider.0,
            &mut raw,
            BCRYPT_RSA_ALGORITHM,
            TPM_KEY_NAME,
            0,
            0,
        )
    };
    if result == NTE_EXISTS {
        // A concurrent creator won. One reopen, no polling or replacement.
        return open_key(provider);
    }
    status("TPM create", result)?;
    let handle = Handle(raw);
    set_u32(&handle, NCRYPT_LENGTH_PROPERTY, 2048)?;
    set_u32(
        &handle,
        NCRYPT_KEY_USAGE_PROPERTY,
        NCRYPT_ALLOW_DECRYPT_FLAG,
    )?;
    // The Platform Provider defaults to a non-exportable key, as verified by the
    // probe. validate_key checks policy 0 before use instead of changing policy.
    // SAFETY: handle is an unfinalized key owned by this scope.
    status("TPM finalize", unsafe {
        NCryptFinalizeKey(handle.0, NCRYPT_SILENT_FLAG)
    })?;
    Ok(handle)
}

fn set_u32(handle: &Handle, property: PCWSTR, value: u32) -> Result<(), Error> {
    let bytes = value.to_le_bytes();
    // SAFETY: live handle, static property name and a 4-byte input valid for this call.
    status("TPM set property", unsafe {
        NCryptSetProperty(handle.0, property, bytes.as_ptr(), 4, 0)
    })
}

fn get_u32(handle: &Handle, property: PCWSTR) -> Result<u32, Error> {
    let mut bytes = [0; 4];
    let mut written = 0;
    // SAFETY: live handle, static property name and correctly sized writable buffers.
    status("TPM get property", unsafe {
        NCryptGetProperty(handle.0, property, bytes.as_mut_ptr(), 4, &mut written, 0)
    })?;
    if written != 4 {
        return Err(Error::InvalidFormat);
    }
    Ok(u32::from_le_bytes(bytes))
}

fn validate_key(handle: &Handle) -> Result<(), Error> {
    if get_u32(handle, NCRYPT_LENGTH_PROPERTY)? != 2048
        || get_u32(handle, NCRYPT_EXPORT_POLICY_PROPERTY)? != 0
        // Measured on a real TPM: a key created as decrypt-only reads back as
        // decrypt + signing (3), so require the decrypt bit instead of equality.
        || get_u32(handle, NCRYPT_KEY_USAGE_PROPERTY)? & NCRYPT_ALLOW_DECRYPT_FLAG == 0
    {
        return Err(Error::InvalidFormat);
    }
    Ok(())
}

fn crypt(handle: &Handle, input: &[u8], decrypt: bool) -> Result<Zeroing<Vec<u8>>, Error> {
    if (decrypt && input.len() != 256) || (!decrypt && input.len() != 32) {
        return Err(Error::InvalidFormat);
    }
    let padding = BCRYPT_OAEP_PADDING_INFO {
        pszAlgId: BCRYPT_SHA256_ALGORITHM,
        pbLabel: null_mut(),
        cbLabel: 0,
    };
    let padding_ptr = (&raw const padding).cast::<c_void>();
    // Fixed 512-byte native scratch buffer, as in the verified TPM probe.
    // Zeroing also clears plaintext on native failures and invalid output lengths.
    let mut output = Zeroing::new(vec![0; 512]);
    let mut written = 0;
    // SAFETY: handle is live, all pointers address live buffers with their exact
    // sizes, padding remains valid for the synchronous call, input is read-only.
    let result = unsafe {
        if decrypt {
            NCryptDecrypt(
                handle.0,
                input.as_ptr(),
                input.len() as u32,
                padding_ptr,
                output.as_mut_ptr(),
                output.len() as u32,
                &mut written,
                NCRYPT_PAD_OAEP_FLAG | NCRYPT_SILENT_FLAG,
            )
        } else {
            NCryptEncrypt(
                handle.0,
                input.as_ptr(),
                input.len() as u32,
                padding_ptr,
                output.as_mut_ptr(),
                output.len() as u32,
                &mut written,
                NCRYPT_PAD_OAEP_FLAG | NCRYPT_SILENT_FLAG,
            )
        }
    };
    status(
        if decrypt {
            "TPM decrypt"
        } else {
            "TPM encrypt"
        },
        result,
    )?;
    if written as usize > output.len() {
        return Err(Error::InvalidFormat);
    }
    // Copy only the result; the full scratch buffer is wiped by its owner.
    Ok(Zeroing::new(output[..written as usize].to_vec()))
}

struct LocalBlob(CRYPT_INTEGER_BLOB);

impl Drop for LocalBlob {
    fn drop(&mut self) {
        if !self.0.pbData.is_null() {
            // SAFETY: DPAPI allocated cbData bytes with LocalAlloc. This unique
            // owner zeroes them before releasing with the matching LocalFree.
            unsafe {
                let bytes = std::slice::from_raw_parts_mut(self.0.pbData, self.0.cbData as usize);
                wipe(bytes);
                LocalFree(self.0.pbData.cast());
            }
        }
    }
}

pub(super) fn protect(input: &[u8]) -> Result<Vec<u8>, Error> {
    // Output here is ciphertext. The zeroing owner also covers early errors.
    Ok(transform(input, true)?.to_vec())
}

pub(super) fn unprotect(input: &[u8]) -> Result<Zeroing<Vec<u8>>, Error> {
    transform(input, false)
}

fn transform(input: &[u8], protect: bool) -> Result<Zeroing<Vec<u8>>, Error> {
    if input.is_empty() || input.len() > MAX_FILE_SIZE {
        return Err(Error::InvalidFormat);
    }
    let source = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr().cast_mut(),
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr().cast_mut(),
    };
    let mut output = LocalBlob(CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    });
    // SAFETY: source and entropy are read-only live buffers. Output is writable,
    // optional pointers are null. No machine scope flag: current-user DPAPI only.
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &source,
                null(),
                &entropy,
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        } else {
            CryptUnprotectData(
                &source,
                null_mut(),
                &entropy,
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        }
    };
    if ok == 0 {
        return Err(Error::Native {
            operation: if protect {
                "DPAPI protect"
            } else {
                "DPAPI unprotect"
            },
            code: io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32,
        });
    }
    if output.0.pbData.is_null() || output.0.cbData == 0 || output.0.cbData as usize > MAX_FILE_SIZE
    {
        return Err(Error::InvalidFormat);
    }
    // SAFETY: successful DPAPI output describes a valid cbData-byte allocation.
    // This bounded copy is owned by Zeroing; LocalBlob clears the native original.
    let bytes = unsafe { std::slice::from_raw_parts(output.0.pbData, output.0.cbData as usize) };
    Ok(Zeroing::new(bytes.to_vec()))
}

pub(super) fn publish(source: &Path, destination: &Path) -> Result<(), Error> {
    // Windows paths are OS-selected/caller-supplied, not retained caches. Bound the
    // FFI buffers to Windows' extended-path maximum of 32,767 UTF-16 units.
    fn wide(path: &Path) -> Result<Vec<u16>, Error> {
        let mut value = Vec::with_capacity(32768);
        for unit in path.as_os_str().encode_wide() {
            if unit == 0 || value.len() == 32767 {
                return Err(Error::UnsafeDataDirectory);
            }
            value.push(unit);
        }
        value.push(0);
        Ok(value)
    }
    let source = wide(source)?;
    let destination = wide(destination)?;
    // SAFETY: both paths are live NUL-terminated UTF-16 strings. Same-directory
    // rename, no COPY_ALLOWED or REPLACE_EXISTING: an existing target is preserved.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_clears_owned_bytes_on_success_and_error() {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).unwrap();
        {
            let _owner = Zeroing::new(bytes.as_mut_slice());
        }
        assert!(bytes.iter().all(|byte| *byte == 0));
        fn early_error(bytes: &mut [u8]) -> Result<(), Error> {
            let _owner = Zeroing::new(bytes);
            Err(Error::InvalidFormat)
        }
        getrandom::fill(&mut bytes).unwrap();
        assert!(early_error(&mut bytes).is_err());
        assert!(bytes.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn only_hardware_unavailability_allows_fallback() {
        assert!(unavailable(TBS_E_TPM_NOT_FOUND));
        assert!(unavailable(NTE_DEVICE_NOT_READY));
        assert!(unavailable(NTE_NOT_SUPPORTED));
        assert!(unavailable(TPM_E_DISABLED));
        assert!(!unavailable(NTE_BAD_KEYSET));
        assert!(!unavailable(windows_sys::Win32::Foundation::TPM_E_FAIL));
        assert!(!unavailable(0x80090010u32 as i32)); // NTE_PERM
    }
}
