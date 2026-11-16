//! Disposable probe for the TPM step of the key-wrapping chain. Creates a persistent,
//! non-exportable RSA-2048 key in the Platform Crypto Provider, wraps and unwraps 32 bytes with
//! OAEP, tries to export the private key (must fail) and deletes the key again.
//! Prints only status codes, sizes and timings; never key material.

use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::time::Instant;

use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_OAEP_PADDING_INFO, BCRYPT_RSA_ALGORITHM, BCRYPT_RSAPRIVATE_BLOB, BCRYPT_SHA1_ALGORITHM,
    BCRYPT_SHA256_ALGORITHM, MS_PLATFORM_CRYPTO_PROVIDER, NCRYPT_ALLOW_DECRYPT_FLAG,
    NCRYPT_EXPORT_POLICY_PROPERTY, NCRYPT_KEY_USAGE_PROPERTY, NCRYPT_LENGTH_PROPERTY,
    NCRYPT_PAD_OAEP_FLAG, NCryptCreatePersistedKey, NCryptDecrypt, NCryptDeleteKey, NCryptEncrypt,
    NCryptExportKey, NCryptFinalizeKey, NCryptFreeObject, NCryptGetProperty, NCryptOpenKey,
    NCryptOpenStorageProvider, NCryptSetProperty,
};
use windows_sys::core::{PCWSTR, w};

const KEY_NAME: PCWSTR = w!("tg-rust-tpm-probe");

fn hex(status: i32) -> String {
    format!("0x{:08X}", status as u32)
}

fn set_u32(handle: usize, property: PCWSTR, value: u32) -> i32 {
    let bytes = value.to_le_bytes();
    // SAFETY: `bytes` outlives the call and its length is passed correctly.
    unsafe { NCryptSetProperty(handle, property, bytes.as_ptr(), 4, 0) }
}

fn crypt(key: usize, hash: PCWSTR, input: &[u8], decrypt: bool) -> Result<Vec<u8>, i32> {
    let padding = BCRYPT_OAEP_PADDING_INFO { pszAlgId: hash, pbLabel: null_mut(), cbLabel: 0 };
    let padding_ptr = (&raw const padding).cast::<c_void>();
    let mut out = vec![0u8; 512];
    let mut written = 0u32;
    // SAFETY: all pointers refer to live local buffers with correct lengths.
    let status = unsafe {
        if decrypt {
            NCryptDecrypt(
                key,
                input.as_ptr(),
                input.len() as u32,
                padding_ptr,
                out.as_mut_ptr(),
                out.len() as u32,
                &mut written,
                NCRYPT_PAD_OAEP_FLAG,
            )
        } else {
            NCryptEncrypt(
                key,
                input.as_ptr(),
                input.len() as u32,
                padding_ptr,
                out.as_mut_ptr(),
                out.len() as u32,
                &mut written,
                NCRYPT_PAD_OAEP_FLAG,
            )
        }
    };
    if status != 0 {
        return Err(status);
    }
    out.truncate(written as usize);
    Ok(out)
}

fn main() {
    let mut provider = 0usize;
    // SAFETY: out pointer is a live local.
    let status =
        unsafe { NCryptOpenStorageProvider(&mut provider, MS_PLATFORM_CRYPTO_PROVIDER, 0) };
    println!("open provider: {}", hex(status));
    if status != 0 {
        return;
    }

    let mut key = 0usize;
    // Remove a leftover key from an interrupted earlier run.
    // SAFETY: out pointer is a live local; the handle is consumed by NCryptDeleteKey.
    unsafe {
        if NCryptOpenKey(provider, &mut key, KEY_NAME, 0, 0) == 0 {
            println!("leftover key deleted: {}", hex(NCryptDeleteKey(key, 0)));
        }
    }

    let started = Instant::now();
    // SAFETY: out pointer is a live local.
    let status = unsafe {
        NCryptCreatePersistedKey(provider, &mut key, BCRYPT_RSA_ALGORITHM, KEY_NAME, 0, 0)
    };
    println!("create key: {}", hex(status));
    if status != 0 {
        return;
    }
    println!("set length 2048: {}", hex(set_u32(key, NCRYPT_LENGTH_PROPERTY, 2048)));
    println!(
        "set usage decrypt-only: {}",
        hex(set_u32(key, NCRYPT_KEY_USAGE_PROPERTY, NCRYPT_ALLOW_DECRYPT_FLAG))
    );
    // SAFETY: `key` is a valid handle from NCryptCreatePersistedKey.
    let status = unsafe { NCryptFinalizeKey(key, 0) };
    println!("finalize: {} ({} ms)", hex(status), started.elapsed().as_millis());
    if status != 0 {
        return;
    }

    let mut policy = [0u8; 4];
    let mut size = 0u32;
    // SAFETY: buffer and size pointers are live locals.
    let status = unsafe {
        NCryptGetProperty(key, NCRYPT_EXPORT_POLICY_PROPERTY, policy.as_mut_ptr(), 4, &mut size, 0)
    };
    println!("export policy: {} value {}", hex(status), u32::from_le_bytes(policy));

    for (name, prop) in [("length", NCRYPT_LENGTH_PROPERTY), ("usage", NCRYPT_KEY_USAGE_PROPERTY)] {
        let mut v = [0u8; 4];
        // SAFETY: buffer and size pointers are live locals.
        let st = unsafe { NCryptGetProperty(key, prop, v.as_mut_ptr(), 4, &mut size, 0) };
        println!("{name}: {} value {} size {size}", hex(st), u32::from_le_bytes(v));
    }

    let mut blob = vec![0u8; 4096];
    // SAFETY: buffer and size pointers are live locals.
    let status = unsafe {
        NCryptExportKey(
            key,
            0,
            BCRYPT_RSAPRIVATE_BLOB,
            null(),
            blob.as_mut_ptr(),
            blob.len() as u32,
            &mut size,
            0,
        )
    };
    println!("export private key (must fail): {}", hex(status));

    let secret: Vec<u8> = (0u8..32).collect();
    for (name, hash) in [("SHA-256", BCRYPT_SHA256_ALGORITHM), ("SHA-1", BCRYPT_SHA1_ALGORITHM)] {
        match crypt(key, hash, &secret, false) {
            Err(status) => println!("OAEP {name}: encrypt failed {}", hex(status)),
            Ok(wrapped) => {
                let started = Instant::now();
                match crypt(key, hash, &wrapped, true) {
                    Err(status) => println!("OAEP {name}: decrypt failed {}", hex(status)),
                    Ok(plain) => println!(
                        "OAEP {name}: wrapped {} bytes, round trip {}, decrypt {} ms",
                        wrapped.len(),
                        if plain == secret { "OK" } else { "MISMATCH" },
                        started.elapsed().as_millis()
                    ),
                }
            }
        }
    }

    // Reopen by name to confirm the key is persistent, then delete it.
    // SAFETY: handles are valid; NCryptDeleteKey consumes the key handle.
    unsafe {
        NCryptFreeObject(key);
        let status = NCryptOpenKey(provider, &mut key, KEY_NAME, 0, 0);
        println!("reopen by name: {}", hex(status));
        if status == 0 {
            println!("delete key: {}", hex(NCryptDeleteKey(key, 0)));
        }
        NCryptFreeObject(provider);
    }
}
