//! Record the TDLib installation without linking the DLL into test executables.
use std::path::{Component, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=TDLIB_DIR");
    // Default: a TDLib build next to the repository directory (see docs/SETUP.md).
    let directory = std::env::var("TDLIB_DIR").unwrap_or_else(|_| {
        let mut path = PathBuf::new();
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for component in manifest.join(r"..\..\..\tdlib\install").components() {
            match component {
                Component::ParentDir => {
                    path.pop();
                }
                other => path.push(other),
            }
        }
        path.to_string_lossy().into_owned()
    });
    println!("cargo:rustc-env=TG_TDLIB_DIR={directory}");
}
