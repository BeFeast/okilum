use std::{env, path::PathBuf, process::Command};

fn run(command: &mut Command) {
    assert!(
        command
            .status()
            .expect("cannot invoke macOS compiler")
            .success(),
        "macOS bridge build failed"
    );
}

/// Compiles the Sparkle bridge and links the vendored stock Sparkle.framework.
fn main() {
    for name in [
        "TESSERA_RELEASE_VERSION",
        "TESSERA_SOURCE_COMMIT",
        "TESSERA_BUILD_VERSION",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
        let value = env::var(name).unwrap_or_else(|_| {
            if name == "TESSERA_RELEASE_VERSION" {
                format!("{} (development)", env!("CARGO_PKG_VERSION"))
            } else if name == "TESSERA_BUILD_VERSION" {
                "development".to_string()
            } else {
                "unknown (development)".to_string()
            }
        });
        println!("cargo:rustc-env={name}={value}");
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-env-changed=TESSERA_WINDOWS_ICON");
        let icon = env::var("TESSERA_WINDOWS_ICON")
            .expect("Run scripts/build-windows-ci.sh to generate the Windows icon");
        let version = env::var("TESSERA_RELEASE_VERSION")
            .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").into());
        let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
        let resource = out.join("tessera.rc");
        let template = std::fs::read_to_string("resources/windows/tessera.rc").unwrap();
        std::fs::write(
            &resource,
            template
                .replace("@ICON@", &icon.replace('\\', "/"))
                .replace("@VERSION@", &version),
        )
        .unwrap();
        println!("cargo:rerun-if-changed=resources/windows/tessera.rc");
        println!("cargo:rerun-if-changed={icon}");
        embed_resource::compile(&resource, embed_resource::NONE)
            .manifest_required()
            .expect("Windows resources must compile");
    }
    println!("cargo:rerun-if-changed=src/updater/bridge.m");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let framework =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../vendor/sparkle");
    assert!(
        framework
            .join("Sparkle.framework/Headers/Sparkle.h")
            .is_file(),
        "Run scripts/updater/sparkle.py prepare with the pinned archive before building macOS"
    );
    assert_eq!(
        env::var("CARGO_CFG_TARGET_ARCH").as_deref(),
        Ok("aarch64"),
        "Updater packaging currently supports macOS arm64 only"
    );
    println!("cargo:rerun-if-changed=../../vendor/sparkle/Sparkle.framework");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed=src/thumbnail/bridge.m");
    let thumbnail = out.join("thumbnail.o");
    run(Command::new("xcrun")
        .args([
            "clang",
            "-fobjc-arc",
            "-fblocks",
            "-Wall",
            "-Werror",
            "-arch",
            "arm64",
            "-mmacosx-version-min=11.0",
            "-c",
            "src/thumbnail/bridge.m",
            "-o",
        ])
        .arg(&thumbnail));
    let object = out.join("updater.o");
    run(Command::new("xcrun")
        .args([
            "clang",
            "-fobjc-arc",
            "-fblocks",
            "-arch",
            "arm64",
            "-mmacosx-version-min=11.0",
            "-F",
        ])
        .arg(&framework)
        .args(["-c", "src/updater/bridge.m", "-o"])
        .arg(&object));
    run(Command::new("xcrun")
        .args(["ar", "crs"])
        .arg(out.join("libtessera_updater.a"))
        .arg(object)
        .arg(thumbnail));
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=tessera_updater");
    println!("cargo:rustc-link-search=framework={}", framework.display());
    println!("cargo:rustc-link-lib=framework=Sparkle");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=QuickLookThumbnailing");
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
}
