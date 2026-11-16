//! Embed the Windows resources (the application icon) into tg-app.exe.
fn main() {
    println!("cargo:rerun-if-changed=assets/ferrogram.rc");
    println!("cargo:rerun-if-changed=assets/ferrogram.ico");
    if let Err(error) =
        embed_resource::compile("assets/ferrogram.rc", embed_resource::NONE).manifest_optional()
    {
        println!("cargo:warning=Could not embed the application icon: {error}");
        std::process::exit(1);
    }
}
