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
    // All desktop platforms mount the same retained native Manager session.
    println!("cargo:rustc-check-cfg=cfg(native_manager)");
    if matches!(
        env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("linux" | "macos" | "windows")
    ) {
        println!("cargo:rustc-cfg=native_manager");
    }
    println!("cargo:rerun-if-env-changed=OKILUM_SPRINT_BUILD");
    if env::var_os("CARGO_FEATURE_LINUX_SPRINT").is_some() {
        assert!(
            env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
                && env::var("OKILUM_SPRINT_BUILD").as_deref() == Ok("1")
                && env::var("OKILUM_RELEASE_VERSION").is_ok_and(|v| v.starts_with("Sprint ")),
            "linux-sprint is reserved for the explicit Linux Sprint producer, not ordinary/release builds"
        );
    }
    for name in [
        "OKILUM_RELEASE_VERSION",
        "OKILUM_SOURCE_COMMIT",
        "OKILUM_BUILD_VERSION",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
        let value = env::var(name).unwrap_or_else(|_| {
            if name == "OKILUM_RELEASE_VERSION" {
                format!("{} (development)", env!("CARGO_PKG_VERSION"))
            } else if name == "OKILUM_BUILD_VERSION" {
                "development".to_string()
            } else {
                "unknown (development)".to_string()
            }
        });
        println!("cargo:rustc-env={name}={value}");
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-env-changed=OKILUM_WINDOWS_ICON");
        let icon = env::var("OKILUM_WINDOWS_ICON")
            .expect("Run scripts/build-windows-ci.sh to generate the Windows icon");
        let version =
            env::var("OKILUM_RELEASE_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").into());
        let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
        let resource = out.join("okilum.rc");
        let template = std::fs::read_to_string("resources/windows/okilum.rc").unwrap();
        std::fs::write(
            &resource,
            template
                .replace("@ICON@", &icon.replace('\\', "/"))
                .replace("@FILEVERSION@", &numeric_version(&version))
                .replace("@VERSION@", &version),
        )
        .unwrap();
        println!("cargo:rerun-if-changed=resources/windows/okilum.rc");
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
    let architecture = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => panic!("Unsupported macOS architecture: {other:?}"),
    };
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
            architecture,
            "-mmacosx-version-min=11.0",
            "-c",
            "src/thumbnail/bridge.m",
            "-o",
        ])
        .arg(&thumbnail));
    println!("cargo:rerun-if-changed=src/quicklook/bridge.m");
    let quicklook = out.join("quicklook.o");
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
            "src/quicklook/bridge.m",
            "-o",
        ])
        .arg(&quicklook));
    println!("cargo:rerun-if-changed=src/project/desktop/macos.m");
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
    println!("cargo:rerun-if-changed=src/folder_service/bridge.m");
    let folder_service = out.join("folder_service.o");
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
            "src/folder_service/bridge.m",
            "-o",
        ])
        .arg(&folder_service));
    let object = out.join("updater.o");
    run(Command::new("xcrun")
        .args([
            "clang",
            "-fobjc-arc",
            "-fblocks",
            "-arch",
            architecture,
            "-mmacosx-version-min=11.0",
            "-F",
        ])
        .arg(&framework)
        .args(["-c", "src/updater/bridge.m", "-o"])
        .arg(&object));
    run(Command::new("xcrun")
        .args(["ar", "crs"])
        .arg(out.join("libokilum_updater.a"))
        .arg(object)
        .arg(thumbnail)
        .arg(quicklook)
        .arg(t3_desktop)
        .arg(folder_service));
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=okilum_updater");
    println!("cargo:rustc-link-search=framework={}", framework.display());
    println!("cargo:rustc-link-lib=framework=Sparkle");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=Quartz");
    println!("cargo:rustc-link-lib=framework=QuickLookThumbnailing");
    // With a library target Cargo attaches rustc-link-lib to that target only.
    // The existing binary calls these FFI bridges directly and does not yet use
    // the new shared component library. Preserve its native links explicitly,
    // including the binary's libtest harness, without changing main.rs.
    println!("cargo:rustc-link-arg-bin=okilum=-lokilum_updater");
    for name in ["Sparkle", "AppKit", "Quartz", "QuickLookThumbnailing"] {
        println!("cargo:rustc-link-arg-bin=okilum=-Wl,-framework,{name}");
    }
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
}

/// The VERSIONINFO numeric form of a release version (#1104): `0.1.33094` becomes
/// `0,1,33094,0`, so file properties show the build, not `0.1.0.0`.
/// This is the version the binary was compiled as: a Stable re-sign packs an earlier
/// build's exact payload under a new number and keeps that build's VERSIONINFO
/// (docs/windows-delivery.md, "VERSIONINFO").
/// Each field is the leading digits of a dot-separated part; a field above 65535
/// cannot be represented, and the package version is used instead.
fn numeric_version(version: &str) -> String {
    let fields = |text: &str| -> Option<Vec<u16>> {
        let mut out = Vec::new();
        for part in text.split_whitespace().next()?.split('.').take(4) {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            out.push(if digits.is_empty() {
                0
            } else {
                digits.parse().ok()?
            });
        }
        out.resize(4, 0);
        Some(out)
    };
    let fields = fields(version).unwrap_or_else(|| {
        println!(
            "cargo:warning=version {version:?} does not fit VERSIONINFO; using the package version"
        );
        fields(env!("CARGO_PKG_VERSION")).unwrap()
    });
    fields
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",")
}
