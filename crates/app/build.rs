//! Embed the Windows resources (the application icon) into tg-app.exe.
fn main() {
    println!("cargo:rerun-if-changed=assets/veygram.rc");
    println!("cargo:rerun-if-changed=assets/veygram.ico");
    if let Err(error) =
        embed_resource::compile("assets/veygram.rc", embed_resource::NONE).manifest_optional()
    {
        println!("cargo:warning=Could not embed the application icon: {error}");
        std::process::exit(1);
    }
}
