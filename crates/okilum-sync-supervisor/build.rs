use std::{env, path::PathBuf};

/// Windows: the same version (and icon, when the build provides one) as the
/// app, so the helper is not a bare file name in Task Manager (#1037).
fn main() {
    println!("cargo:rerun-if-env-changed=OKILUM_RELEASE_VERSION");
    println!("cargo:rerun-if-env-changed=OKILUM_WINDOWS_ICON");
    println!("cargo:rerun-if-changed=resources/windows/okilum-sync.rc");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version =
        env::var("OKILUM_RELEASE_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").into());
    let icon = env::var("OKILUM_WINDOWS_ICON")
        .map(|icon| {
            println!("cargo:rerun-if-changed={icon}");
            format!("1 ICON \"{}\"", icon.replace('\\', "/"))
        })
        .unwrap_or_default();
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let resource = out.join("okilum-sync.rc");
    let template = std::fs::read_to_string("resources/windows/okilum-sync.rc").unwrap();
    std::fs::write(
        &resource,
        template
            .replace("@ICON_LINE@", &icon)
            .replace("@FILEVERSION@", &numeric_version(&version))
            .replace("@VERSION@", &version),
    )
    .unwrap();
    embed_resource::compile(&resource, embed_resource::NONE)
        .manifest_optional()
        .expect("Windows resources must compile");
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
