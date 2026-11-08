fn main() {
    println!("cargo:rerun-if-changed=native/macos_listener.c");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("native/macos_listener.c")
            .warnings_into_errors(true)
            .compile("okilum_macos_listener");
    }
}
