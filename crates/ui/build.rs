fn main() {
    slint_build::compile("ui/app.slint").unwrap_or_else(|_| panic!("UI compilation failed"));
}
