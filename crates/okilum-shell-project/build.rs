use std::{env, path::PathBuf, process::Command};

fn run(command: &mut Command) {
    assert!(
        command
            .status()
            .expect("cannot invoke macOS compiler")
            .success(),
        "macOS project desktop bridge build failed"
    );
}

fn main() {
    // All desktop platforms mount the same retained native Manager session.
    println!("cargo:rustc-check-cfg=cfg(native_manager)");
    if matches!(
        env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("linux" | "macos" | "windows")
    ) {
        println!("cargo:rustc-cfg=native_manager");
    }
    println!("cargo:rerun-if-changed=src/project/desktop/macos.m");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let architecture = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => panic!("Unsupported macOS architecture: {other:?}"),
    };
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let t3_desktop = out.join("t3_desktop.o");
    run(Command::new("xcrun")
        .args([
            "clang",
            "-fobjc-arc",
            "-Wall",
            "-Werror",
            "-arch",
            architecture,
            "-mmacosx-version-min=11.0",
            "-c",
            "src/project/desktop/macos.m",
            "-o",
        ])
        .arg(&t3_desktop));
    run(Command::new("xcrun")
        .args(["ar", "crs"])
        .arg(out.join("libokilum_project_desktop.a"))
        .arg(t3_desktop));
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=okilum_project_desktop");
    println!("cargo:rustc-link-lib=framework=AppKit");
}
