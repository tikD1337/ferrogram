//! Points the linker at the TDLib import library built by `scripts/build-tdlib.ps1`.

fn main() {
    println!("cargo:rerun-if-env-changed=TDLIB_DIR");
    let root = std::env::var("TDLIB_DIR").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), r"\..\..\..\tdlib\install").to_owned());
    println!("cargo:rustc-link-search=native={root}\\lib");
}
