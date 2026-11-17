use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "security-test-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

struct DpapiOnly;

impl Protection for DpapiOnly {
    fn tpm_wrap(&self, _: &[u8; 32]) -> Result<Option<Vec<u8>>, Error> {
        Ok(None)
    }

    fn tpm_unwrap(&self, _: &[u8]) -> Result<DatabaseKey, Error> {
        panic!("DPAPI-only files must never call the TPM")
    }

    fn protect(&self, input: &[u8]) -> Result<Vec<u8>, Error> {
        ffi::protect(input)
    }

    fn unprotect(&self, input: &[u8]) -> Result<Zeroing<Vec<u8>>, Error> {
        ffi::unprotect(input)
    }
}

#[test]
fn fallback_round_trip_and_redaction() {
    let dir = TestDir::new();
    let created = load_or_create_with(&dir.0, &DpapiOnly).unwrap();
    assert!(!created.tpm_protected);
    assert_eq!(created.key.expose().len(), 32);
    assert_eq!(format!("{:?}", created.key), "DatabaseKey(<redacted>)");
    assert_eq!(
        format!("{created:?}"),
        "KeyProtection { key: DatabaseKey(<redacted>), tpm_protected: false }"
    );
    let loaded = load_or_create_with(&dir.0, &DpapiOnly).unwrap();
    assert!(!loaded.tpm_protected);
    assert!(loaded.key.expose() == created.key.expose());
}

#[test]
fn malformed_files_are_never_replaced() {
    let dir = TestDir::new();
    let _key = load_or_create_with(&dir.0, &DpapiOnly).unwrap();
    let path = dir.0.join(KEY_FILE_NAME);
    let valid = fs::read(&path).unwrap();
    let mut cases = vec![Vec::new(), valid[..HEADER_LEN].to_vec()];
    for offset in [0, MAGIC.len(), HEADER_LEN - 1, valid.len() - 1] {
        let mut damaged = valid.clone();
        damaged[offset] ^= 0x7f;
        cases.push(damaged);
    }
    cases.push(vec![0; MAX_FILE_SIZE + 1]);
    for damaged in cases {
        fs::write(&path, &damaged).unwrap();
        assert!(load_or_create_with(&dir.0, &DpapiOnly).is_err());
        assert!(fs::read(&path).unwrap() == damaged);
    }
    let mut future = valid;
    future[MAGIC.len()] = VERSION + 1;
    fs::write(&path, &future).unwrap();
    assert!(matches!(
        load_or_create_with(&dir.0, &DpapiOnly),
        Err(Error::UnsupportedVersion(_))
    ));
    assert!(fs::read(path).unwrap() == future);
}

#[test]
fn mode_is_bound_to_dpapi_plaintext() {
    let dir = TestDir::new();
    let _key = load_or_create_with(&dir.0, &DpapiOnly).unwrap();
    let path = dir.0.join(KEY_FILE_NAME);
    let mut bytes = fs::read(&path).unwrap();
    bytes[HEADER_LEN - 1] = MODE_TPM;
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        load_or_create_with(&dir.0, &DpapiOnly),
        Err(Error::InvalidFormat)
    ));
    assert!(fs::read(path).unwrap() == bytes);
}

#[test]
fn publishing_never_overwrites_an_existing_file() {
    let dir = TestDir::new();
    let path = dir.0.join(KEY_FILE_NAME);
    fs::write(&path, b"unrelated synthetic data").unwrap();
    assert!(publish(&path, b"replacement synthetic data").is_err());
    assert!(fs::read(&path).unwrap() == b"unrelated synthetic data");
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[test]
fn rejects_probe_directories_and_relative_paths() {
    assert!(validate_directory(Path::new("relative")).is_err());
    let root = std::env::current_dir().unwrap();
    for name in ["tdlib-data", "TDLIB-DATA"] {
        assert!(validate_directory(&root.join("spikes").join(name)).is_err());
    }
}

#[test]
fn data_directory_selection_respects_override_and_has_no_probe_default() {
    let root = std::env::current_dir().unwrap();
    let local = root.join("synthetic-local-app-data");
    let custom = root.join("synthetic-custom-data");
    assert_eq!(
        select_data_dir(None, Some(local.clone().into_os_string())).unwrap(),
        local.join("tg-rust")
    );
    assert_eq!(
        select_data_dir(Some(custom.clone().into_os_string()), None).unwrap(),
        custom
    );
    assert!(select_data_dir(None, None).is_err());
    assert!(select_data_dir(Some("".into()), Some(local.into_os_string())).is_err());
}

struct FailedTpm;

impl Protection for FailedTpm {
    fn tpm_wrap(&self, _: &[u8; 32]) -> Result<Option<Vec<u8>>, Error> {
        Err(Error::Native {
            operation: "TPM encrypt",
            code: 0x80090020,
        })
    }

    fn tpm_unwrap(&self, _: &[u8]) -> Result<DatabaseKey, Error> {
        Err(Error::MissingTpmKey)
    }

    fn protect(&self, _: &[u8]) -> Result<Vec<u8>, Error> {
        panic!("TPM failure must not downgrade to DPAPI-only protection")
    }

    fn unprotect(&self, bytes: &[u8]) -> Result<Zeroing<Vec<u8>>, Error> {
        ffi::unprotect(bytes)
    }
}

#[test]
fn tpm_failure_does_not_create_a_fallback_file() {
    let dir = TestDir::new();
    assert!(matches!(
        load_or_create_with(&dir.0, &FailedTpm),
        Err(Error::Native { .. })
    ));
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
}

#[test]
fn missing_tpm_key_never_regenerates_or_downgrades() {
    let dir = TestDir::new();
    // Synthetic wrapped payload; DPAPI is real, only the TPM boundary is injected.
    let mut inner = Vec::from(MAGIC.as_slice());
    inner.extend_from_slice(&[VERSION, MODE_TPM]);
    inner.resize(HEADER_LEN + 256, 0);
    let mut file = inner[..HEADER_LEN].to_vec();
    file.extend_from_slice(&ffi::protect(&inner).unwrap());
    let path = dir.0.join(KEY_FILE_NAME);
    fs::write(&path, &file).unwrap();
    assert!(matches!(
        load_or_create_with(&dir.0, &FailedTpm),
        Err(Error::MissingTpmKey)
    ));
    assert!(fs::read(path).unwrap() == file);
}

#[test]
fn simultaneous_creation_converges_on_one_key() {
    let dir = TestDir::new();
    let barrier = std::sync::Barrier::new(2);
    let (a, b) = std::thread::scope(|scope| {
        let create = || {
            barrier.wait();
            load_or_create_with(&dir.0, &DpapiOnly).unwrap()
        };
        let a = scope.spawn(create);
        let b = scope.spawn(create);
        (a.join().unwrap(), b.join().unwrap())
    });
    assert!(a.key.expose() == b.key.expose());
    assert!(!a.tpm_protected && !b.tpm_protected);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[test]
#[ignore = "Requires a real TPM; creates/reopens the application persistent CNG key"]
fn native_tpm_persistent_round_trip() {
    let dir = TestDir::new();
    let created = load_or_create(&dir.0).unwrap();
    assert!(created.tpm_protected);
    let loaded = load_or_create(&dir.0).unwrap();
    assert!(loaded.tpm_protected);
    assert!(created.key.expose() == loaded.key.expose());
}

#[test]
fn only_key_refusals_offer_a_fresh_start() {
    use std::io::ErrorKind;
    let refusals = [
        Error::MissingTpmKey,
        Error::InvalidFormat,
        Error::UnsupportedVersion(9),
        Error::Native {
            operation: "TPM decrypt",
            code: 0x8028_0084,
        },
        Error::Native {
            operation: "TPM open",
            code: 1,
        },
        Error::Native {
            operation: "TPM provider",
            code: 1,
        },
        Error::Native {
            operation: "DPAPI unprotect",
            code: 13,
        },
    ];
    for error in refusals {
        assert!(error.is_unreadable_key(), "{error:?}");
    }
    // Problems fixed outside the application must never lead to moving the data aside.
    let others = [
        Error::Io(ErrorKind::PermissionDenied),
        Error::Io(ErrorKind::NotFound),
        Error::UnsafeDataDirectory,
        Error::Random,
        Error::Native {
            operation: "TPM create",
            code: 1,
        },
        Error::Native {
            operation: "DPAPI protect",
            code: 1,
        },
    ];
    for error in others {
        assert!(!error.is_unreadable_key(), "{error:?}");
    }
}

#[test]
fn the_key_lives_on_a_locked_page_and_round_trips() {
    let key = DatabaseKey::from_slice(&[7; 32]).expect("key");
    assert_eq!(key.expose(), &[7; 32]);
    // A fresh process has plenty of working-set quota for one page.
    assert!(key.0.is_locked());
    assert!(matches!(
        DatabaseKey::from_slice(&[1; 31]),
        Err(Error::InvalidFormat)
    ));
}
